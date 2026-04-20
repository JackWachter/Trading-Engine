use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use slab::Slab;

use crate::events::{Event, RejectReason};
use crate::types::{
    InstrumentId, NewOrder, OrderId, OrderType, Price, Quantity, Sequence, Side, TimeInForce,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LevelSnapshot {
    pub price: Price,
    pub total_qty: Quantity,
    pub order_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookSnapshot {
    pub best_bid: Option<LevelSnapshot>,
    pub best_ask: Option<LevelSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PriceLevel {
    head: Option<usize>,
    tail: Option<usize>,
    total_qty: Quantity,
    order_count: usize,
}

impl PriceLevel {
    fn new() -> Self {
        Self {
            head: None,
            tail: None,
            total_qty: 0,
            order_count: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OrderNode {
    order_id: OrderId,
    side: Side,
    price: Price,
    original_qty: Quantity,
    remaining_qty: Quantity,
    arrival_sequence: Sequence,
    prev: Option<usize>,
    next: Option<usize>,
}

#[derive(Debug, Default)]
pub struct OrderBook {
    instrument: InstrumentId,
    bids: BTreeMap<Price, PriceLevel>,
    asks: BTreeMap<Price, PriceLevel>,
    orders: Slab<OrderNode>,
    order_index: HashMap<OrderId, usize>,
    next_sequence: Sequence,
}

impl OrderBook {
    pub fn new(instrument: InstrumentId) -> Self {
        Self {
            instrument,
            ..Self::default()
        }
    }

    pub fn instrument(&self) -> InstrumentId {
        self.instrument
    }

    pub fn snapshot(&self) -> BookSnapshot {
        BookSnapshot {
            best_bid: self.best_bid(),
            best_ask: self.best_ask(),
        }
    }

    pub fn submit(&mut self, order: NewOrder) -> Vec<Event> {
        let sequence = self.next_sequence();
        let mut events = Vec::new();

        if self.order_index.contains_key(&order.id) {
            events.push(Event::Rejected {
                instrument: Some(self.instrument),
                order_id: order.id,
                reason: RejectReason::DuplicateOrderId,
                sequence,
            });
            return events;
        }

        if order.quantity == 0 {
            events.push(Event::Rejected {
                instrument: Some(self.instrument),
                order_id: order.id,
                reason: RejectReason::InvalidQuantity,
                sequence,
            });
            return events;
        }

        match order.order_type {
            OrderType::Limit if order.price.is_none() => {
                events.push(Event::Rejected {
                    instrument: Some(self.instrument),
                    order_id: order.id,
                    reason: RejectReason::MissingPrice,
                    sequence,
                });
                return events;
            }
            OrderType::Market if order.price.is_some() => {
                events.push(Event::Rejected {
                    instrument: Some(self.instrument),
                    order_id: order.id,
                    reason: RejectReason::PriceNotAllowedForMarket,
                    sequence,
                });
                return events;
            }
            _ => {}
        }

        events.push(Event::Accepted {
            instrument: self.instrument,
            order_id: order.id,
            sequence,
        });

        let mut remaining = order.quantity;

        while remaining > 0 {
            let Some(best_price) = self.best_opposite_price(order.side) else {
                break;
            };

            if !self.is_crossed(order.side, order.order_type, order.price, best_price) {
                break;
            }

            let maker_index = match self.level_head(order.side.opposite(), best_price) {
                Some(index) => index,
                None => {
                    self.remove_empty_level(order.side.opposite(), best_price);
                    continue;
                }
            };

            let maker = self.orders[maker_index];
            let trade_qty = remaining.min(maker.remaining_qty);
            let maker_remaining = maker.remaining_qty - trade_qty;
            remaining -= trade_qty;

            {
                let maker_mut = self
                    .orders
                    .get_mut(maker_index)
                    .expect("maker order must exist while matching");
                maker_mut.remaining_qty = maker_remaining;
            }

            if let Some(level) = self.level_mut(maker.side, maker.price) {
                level.total_qty -= trade_qty;
            }

            events.push(Event::Trade {
                instrument: self.instrument,
                maker_order_id: maker.order_id,
                taker_order_id: order.id,
                side: order.side,
                price: maker.price,
                quantity: trade_qty,
                maker_remaining,
                taker_remaining: remaining,
                sequence,
            });

            self.push_level_update(&mut events, maker.side, maker.price, sequence);

            if maker_remaining == 0 {
                self.detach_order(maker_index);
                self.order_index.remove(&maker.order_id);
                let _ = self.orders.try_remove(maker_index);
                events.push(Event::Filled {
                    instrument: self.instrument,
                    order_id: maker.order_id,
                    sequence,
                });
                self.remove_empty_level(maker.side, maker.price);
            }
        }

        if remaining == 0 {
            events.push(Event::Filled {
                instrument: self.instrument,
                order_id: order.id,
                sequence,
            });
        } else if matches!(order.order_type, OrderType::Limit)
            && matches!(order.time_in_force, TimeInForce::Gtc)
        {
            let price = order.price.expect("validated limit order must have price");
            self.insert_resting_order(order, remaining, sequence);
            events.push(Event::Rested {
                instrument: self.instrument,
                order_id: order.id,
                side: order.side,
                price,
                remaining,
                sequence,
            });
            self.push_level_update(&mut events, order.side, price, sequence);
        } else {
            events.push(Event::Cancelled {
                instrument: self.instrument,
                order_id: order.id,
                cancelled_qty: remaining,
                sequence,
            });
        }

        self.push_top_of_book(&mut events, sequence);
        events
    }

    pub fn cancel(&mut self, order_id: OrderId) -> Vec<Event> {
        let sequence = self.next_sequence();
        let mut events = Vec::new();

        let Some(index) = self.order_index.remove(&order_id) else {
            events.push(Event::Rejected {
                instrument: Some(self.instrument),
                order_id,
                reason: RejectReason::UnknownOrderId,
                sequence,
            });
            return events;
        };

        let node = self.orders[index];
        let cancelled_qty = node.remaining_qty;

        self.detach_order(index);
        if let Some(level) = self.level_mut(node.side, node.price) {
            level.total_qty -= cancelled_qty;
        }
        self.remove_empty_level(node.side, node.price);
        let _ = self.orders.try_remove(index);

        events.push(Event::Cancelled {
            instrument: self.instrument,
            order_id,
            cancelled_qty,
            sequence,
        });
        self.push_level_update(&mut events, node.side, node.price, sequence);
        self.push_top_of_book(&mut events, sequence);
        events
    }

    fn next_sequence(&mut self) -> Sequence {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        sequence
    }

    pub fn resting_order(&self, order_id: OrderId) -> Option<RestingOrder> {
        let index = *self.order_index.get(&order_id)?;
        let node = self.orders.get(index)?;
        Some(RestingOrder {
            instrument: self.instrument,
            order_id: node.order_id,
            side: node.side,
            price: node.price,
            original_qty: node.original_qty,
            remaining_qty: node.remaining_qty,
            arrival_sequence: node.arrival_sequence,
        })
    }

    pub fn restore_resting_order(
        &mut self,
        order: NewOrder,
        remaining_qty: Quantity,
        arrival_sequence: Sequence,
    ) {
        self.insert_resting_order(order, remaining_qty, arrival_sequence);
        if self.next_sequence <= arrival_sequence {
            self.next_sequence = arrival_sequence + 1;
        }
    }

    fn best_bid(&self) -> Option<LevelSnapshot> {
        self.bids
            .last_key_value()
            .map(|(&price, level)| LevelSnapshot {
                price,
                total_qty: level.total_qty,
                order_count: level.order_count,
            })
    }

    fn best_ask(&self) -> Option<LevelSnapshot> {
        self.asks
            .first_key_value()
            .map(|(&price, level)| LevelSnapshot {
                price,
                total_qty: level.total_qty,
                order_count: level.order_count,
            })
    }

    fn best_opposite_price(&self, taker_side: Side) -> Option<Price> {
        match taker_side {
            Side::Bid => self.asks.first_key_value().map(|(&price, _)| price),
            Side::Ask => self.bids.last_key_value().map(|(&price, _)| price),
        }
    }

    fn is_crossed(
        &self,
        side: Side,
        order_type: OrderType,
        limit_price: Option<Price>,
        best_opposite_price: Price,
    ) -> bool {
        match order_type {
            OrderType::Market => true,
            OrderType::Limit => match side {
                Side::Bid => limit_price.is_some_and(|price| price >= best_opposite_price),
                Side::Ask => limit_price.is_some_and(|price| price <= best_opposite_price),
            },
        }
    }

    fn insert_resting_order(
        &mut self,
        order: NewOrder,
        remaining_qty: Quantity,
        arrival_sequence: Sequence,
    ) {
        let price = order.price.expect("resting order must have a price");
        let index = self.orders.insert(OrderNode {
            order_id: order.id,
            side: order.side,
            price,
            original_qty: order.quantity,
            remaining_qty,
            arrival_sequence,
            prev: None,
            next: None,
        });

        self.order_index.insert(order.id, index);

        let tail = self
            .levels_mut(order.side)
            .entry(price)
            .or_insert_with(PriceLevel::new)
            .tail;

        match tail {
            Some(tail_index) => {
                self.orders[tail_index].next = Some(index);
                self.orders[index].prev = Some(tail_index);
            }
            None => {}
        }

        let level = self
            .levels_mut(order.side)
            .entry(price)
            .or_insert_with(PriceLevel::new);
        match tail {
            Some(_) => level.tail = Some(index),
            None => {
                level.head = Some(index);
                level.tail = Some(index);
            }
        }
        level.total_qty += remaining_qty;
        level.order_count += 1;
    }

    fn detach_order(&mut self, index: usize) {
        let node = self.orders[index];
        let side = node.side;
        let price = node.price;

        if let Some(prev) = node.prev {
            self.orders[prev].next = node.next;
        }
        if let Some(next) = node.next {
            self.orders[next].prev = node.prev;
        }

        if let Some(level) = self.level_mut(side, price) {
            if node.prev.is_none() {
                level.head = node.next;
            }
            if node.next.is_none() {
                level.tail = node.prev;
            }
            level.order_count -= 1;
        }
    }

    fn level_head(&self, side: Side, price: Price) -> Option<usize> {
        self.levels(side).get(&price).and_then(|level| level.head)
    }

    fn level_mut(&mut self, side: Side, price: Price) -> Option<&mut PriceLevel> {
        self.levels_mut(side).get_mut(&price)
    }

    fn levels(&self, side: Side) -> &BTreeMap<Price, PriceLevel> {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    fn levels_mut(&mut self, side: Side) -> &mut BTreeMap<Price, PriceLevel> {
        match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    fn remove_empty_level(&mut self, side: Side, price: Price) {
        let remove = self
            .levels(side)
            .get(&price)
            .is_some_and(|level| level.order_count == 0);
        if remove {
            self.levels_mut(side).remove(&price);
        }
    }

    fn push_level_update(
        &self,
        events: &mut Vec<Event>,
        side: Side,
        price: Price,
        sequence: Sequence,
    ) {
        let event = match self.levels(side).get(&price) {
            Some(level) => Event::LevelUpdated {
                instrument: self.instrument,
                side,
                price,
                total_qty: level.total_qty,
                order_count: level.order_count,
                sequence,
            },
            None => Event::LevelUpdated {
                instrument: self.instrument,
                side,
                price,
                total_qty: 0,
                order_count: 0,
                sequence,
            },
        };
        events.push(event);
    }

    fn push_top_of_book(&self, events: &mut Vec<Event>, sequence: Sequence) {
        events.push(Event::BestBidAskUpdated {
            instrument: self.instrument,
            best_bid: self.best_bid().map(|level| level.price),
            best_ask: self.best_ask().map(|level| level.price),
            sequence,
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestingOrder {
    pub instrument: InstrumentId,
    pub order_id: OrderId,
    pub side: Side,
    pub price: Price,
    pub original_qty: Quantity,
    pub remaining_qty: Quantity,
    pub arrival_sequence: Sequence,
}
