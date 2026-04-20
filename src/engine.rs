use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::book::{BookSnapshot, OrderBook};
use crate::events::{Event, RejectReason};
use crate::risk::{AccountManager, AccountState, Reservation, ReservedOrder};
use crate::types::{
    AccountId, Cash, EngineCommand, InstrumentId, NewOrder, OrderId, Quantity, ReplaceOrder,
    Sequence,
};

#[derive(Debug)]
pub enum AsyncEngineCommand {
    Submit {
        order: NewOrder,
        respond_to: oneshot::Sender<Vec<Event>>,
    },
    Cancel {
        order_id: OrderId,
        respond_to: oneshot::Sender<Vec<Event>>,
    },
    Replace {
        replace: ReplaceOrder,
        respond_to: oneshot::Sender<Vec<Event>>,
    },
    CreditCash {
        account_id: AccountId,
        amount: Cash,
        respond_to: oneshot::Sender<Vec<Event>>,
    },
    CreditPosition {
        account_id: AccountId,
        instrument: InstrumentId,
        quantity: Quantity,
        respond_to: oneshot::Sender<Vec<Event>>,
    },
    Snapshot {
        instrument: InstrumentId,
        respond_to: oneshot::Sender<BookSnapshot>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveOrderState {
    pub order: NewOrder,
    pub remaining_qty: Quantity,
    pub arrival_sequence: Sequence,
    pub reservation: Reservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EngineState {
    pub next_global_sequence: Sequence,
    pub live_orders: Vec<LiveOrderState>,
    pub accounts: HashMap<AccountId, AccountState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveOrderMeta {
    state: LiveOrderState,
}

#[derive(Debug, Default)]
pub struct MatchingEngine {
    books: HashMap<InstrumentId, OrderBook>,
    live_orders: HashMap<OrderId, LiveOrderMeta>,
    order_locations: HashMap<OrderId, InstrumentId>,
    accounts: AccountManager,
    next_global_sequence: Sequence,
}

impl MatchingEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_state(state: EngineState) -> Self {
        let mut engine = Self {
            books: HashMap::new(),
            live_orders: HashMap::new(),
            order_locations: HashMap::new(),
            accounts: AccountManager::from_state(state.accounts),
            next_global_sequence: state.next_global_sequence,
        };

        let mut live_orders = state.live_orders;
        live_orders.sort_by_key(|state| (state.order.instrument, state.arrival_sequence));

        for live in live_orders {
            let instrument = live.order.instrument;
            let book = engine
                .books
                .entry(instrument)
                .or_insert_with(|| OrderBook::new(instrument));
            book.restore_resting_order(live.order, live.remaining_qty, live.arrival_sequence);
            engine.order_locations.insert(live.order.id, instrument);
            engine
                .live_orders
                .insert(live.order.id, LiveOrderMeta { state: live });
        }

        engine
    }

    pub fn export_state(&self) -> EngineState {
        let mut live_orders: Vec<_> = self
            .live_orders
            .values()
            .map(|meta| meta.state.clone())
            .collect();
        live_orders.sort_by_key(|state| (state.order.instrument, state.arrival_sequence));
        EngineState {
            next_global_sequence: self.next_global_sequence,
            live_orders,
            accounts: self.accounts.state().clone(),
        }
    }

    pub fn apply(&mut self, command: EngineCommand) -> Vec<Event> {
        match command {
            EngineCommand::Submit(order) => self.submit(order),
            EngineCommand::Cancel { order_id } => self.cancel(order_id),
            EngineCommand::Replace(replace) => self.replace(replace),
            EngineCommand::CreditCash { account_id, amount } => {
                self.credit_cash(account_id, amount)
            }
            EngineCommand::CreditPosition {
                account_id,
                instrument,
                quantity,
            } => self.credit_position(account_id, instrument, quantity),
        }
    }

    pub fn snapshot(&self, instrument: InstrumentId) -> BookSnapshot {
        self.books
            .get(&instrument)
            .map(OrderBook::snapshot)
            .unwrap_or(BookSnapshot {
                best_bid: None,
                best_ask: None,
            })
    }

    fn submit(&mut self, order: NewOrder) -> Vec<Event> {
        if self.live_orders.contains_key(&order.id) {
            return vec![Event::Rejected {
                instrument: Some(order.instrument),
                order_id: order.id,
                reason: RejectReason::DuplicateOrderId,
                sequence: self.next_reject_sequence(),
            }];
        }

        let snapshot = self.snapshot(order.instrument);
        let reserved = match self.accounts.reserve_for_order(&order, snapshot) {
            Ok(reserved) => reserved,
            Err(reason) => {
                return vec![Event::Rejected {
                    instrument: Some(order.instrument),
                    order_id: order.id,
                    reason,
                    sequence: self.next_reject_sequence(),
                }]
            }
        };

        let events = self
            .books
            .entry(order.instrument)
            .or_insert_with(|| OrderBook::new(order.instrument))
            .submit(order);

        self.apply_submit_effects(order, reserved, &events);
        events
    }

    fn cancel(&mut self, order_id: OrderId) -> Vec<Event> {
        let Some(&instrument) = self.order_locations.get(&order_id) else {
            return vec![Event::Rejected {
                instrument: None,
                order_id,
                reason: RejectReason::UnknownOrderId,
                sequence: self.next_reject_sequence(),
            }];
        };

        let events = self
            .books
            .get_mut(&instrument)
            .expect("order location should point to a live book")
            .cancel(order_id);
        self.apply_terminal_effects(&events);
        events
    }

    fn replace(&mut self, replace: ReplaceOrder) -> Vec<Event> {
        let Some(existing) = self.live_orders.get(&replace.order_id).cloned() else {
            return vec![Event::Rejected {
                instrument: None,
                order_id: replace.order_id,
                reason: RejectReason::UnknownOrderId,
                sequence: self.next_reject_sequence(),
            }];
        };

        if replace.new_quantity == 0 {
            return vec![Event::Rejected {
                instrument: Some(existing.state.order.instrument),
                order_id: replace.order_id,
                reason: RejectReason::InvalidQuantity,
                sequence: self.next_reject_sequence(),
            }];
        }

        let mut events = self.cancel(replace.order_id);
        let replace_sequence = self.next_global_sequence();
        events.push(Event::Replaced {
            instrument: existing.state.order.instrument,
            order_id: replace.order_id,
            old_price: existing.state.order.price.unwrap_or(0),
            new_price: replace.new_price,
            old_remaining: existing.state.remaining_qty,
            new_quantity: replace.new_quantity,
            sequence: replace_sequence,
        });

        let replacement = NewOrder::limit(
            replace.order_id,
            existing.state.order.instrument,
            existing.state.order.side,
            replace.new_price,
            replace.new_quantity,
        )
        .with_account(existing.state.order.account);
        events.extend(self.submit(replacement));
        events
    }

    fn credit_cash(&mut self, account_id: AccountId, amount: Cash) -> Vec<Event> {
        let sequence = self.next_global_sequence();
        let new_cash_balance = self.accounts.credit_cash(account_id, amount);
        vec![Event::CashCredited {
            account_id,
            amount,
            new_cash_balance,
            sequence,
        }]
    }

    fn credit_position(
        &mut self,
        account_id: AccountId,
        instrument: InstrumentId,
        quantity: Quantity,
    ) -> Vec<Event> {
        let sequence = self.next_global_sequence();
        let new_position = self
            .accounts
            .credit_position(account_id, instrument, quantity);
        vec![Event::PositionCredited {
            account_id,
            instrument,
            quantity,
            new_position,
            sequence,
        }]
    }

    fn apply_submit_effects(&mut self, order: NewOrder, reserved: ReservedOrder, events: &[Event]) {
        let mut taker_state = LiveOrderState {
            order,
            remaining_qty: order.quantity,
            arrival_sequence: 0,
            reservation: reserved.reservation,
        };

        for event in events {
            match *event {
                Event::Trade {
                    instrument,
                    maker_order_id,
                    taker_order_id,
                    price,
                    quantity,
                    maker_remaining,
                    taker_remaining,
                    ..
                } => {
                    if taker_order_id == order.id {
                        self.accounts.apply_trade(
                            order.account,
                            instrument,
                            order.side,
                            price,
                            quantity,
                            &mut taker_state.reservation,
                        );
                        taker_state.remaining_qty = taker_remaining;
                    }

                    if let Some(maker) = self.live_orders.get_mut(&maker_order_id) {
                        self.accounts.apply_trade(
                            maker.state.order.account,
                            instrument,
                            maker.state.order.side,
                            price,
                            quantity,
                            &mut maker.state.reservation,
                        );
                        maker.state.remaining_qty = maker_remaining;
                    }
                }
                Event::Rested {
                    instrument,
                    order_id,
                    remaining,
                    sequence,
                    ..
                } if order_id == order.id => {
                    taker_state.remaining_qty = remaining;
                    taker_state.arrival_sequence = sequence;
                    self.order_locations.insert(order.id, instrument);
                    self.live_orders.insert(
                        order.id,
                        LiveOrderMeta {
                            state: taker_state.clone(),
                        },
                    );
                }
                Event::Cancelled { order_id, .. } if order_id == order.id => {
                    self.accounts.release_remaining(
                        order.account,
                        order.instrument,
                        &mut taker_state.reservation,
                    );
                }
                Event::Rejected { order_id, .. } if order_id == order.id => {
                    self.accounts.release_remaining(
                        order.account,
                        order.instrument,
                        &mut taker_state.reservation,
                    );
                }
                Event::Filled { order_id, .. } if order_id == order.id => {}
                Event::Filled { order_id, .. } => {
                    self.remove_live_order(order_id);
                }
                _ => {}
            }
        }
    }

    fn apply_terminal_effects(&mut self, events: &[Event]) {
        for event in events {
            match *event {
                Event::Cancelled {
                    instrument,
                    order_id,
                    ..
                } => {
                    if let Some(mut live) = self.live_orders.remove(&order_id) {
                        self.accounts.release_remaining(
                            live.state.order.account,
                            instrument,
                            &mut live.state.reservation,
                        );
                    }
                    self.order_locations.remove(&order_id);
                }
                Event::Filled { order_id, .. } => {
                    self.remove_live_order(order_id);
                }
                _ => {}
            }
        }
    }

    fn remove_live_order(&mut self, order_id: OrderId) {
        self.live_orders.remove(&order_id);
        self.order_locations.remove(&order_id);
    }

    fn next_global_sequence(&mut self) -> Sequence {
        let sequence = self.next_global_sequence;
        self.next_global_sequence += 1;
        sequence
    }

    fn next_reject_sequence(&mut self) -> Sequence {
        self.next_global_sequence()
    }
}

#[derive(Debug, Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<AsyncEngineCommand>,
    events: broadcast::Sender<Event>,
}

impl EngineHandle {
    pub async fn submit(&self, order: NewOrder) -> Vec<Event> {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(AsyncEngineCommand::Submit {
                order,
                respond_to: tx,
            })
            .await
            .is_err()
        {
            return Vec::new();
        }

        rx.await.unwrap_or_default()
    }

    pub async fn cancel(&self, order_id: OrderId) -> Vec<Event> {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(AsyncEngineCommand::Cancel {
                order_id,
                respond_to: tx,
            })
            .await
            .is_err()
        {
            return Vec::new();
        }

        rx.await.unwrap_or_default()
    }

    pub async fn replace(&self, replace: ReplaceOrder) -> Vec<Event> {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(AsyncEngineCommand::Replace {
                replace,
                respond_to: tx,
            })
            .await
            .is_err()
        {
            return Vec::new();
        }

        rx.await.unwrap_or_default()
    }

    pub async fn credit_cash(&self, account_id: AccountId, amount: Cash) -> Vec<Event> {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(AsyncEngineCommand::CreditCash {
                account_id,
                amount,
                respond_to: tx,
            })
            .await
            .is_err()
        {
            return Vec::new();
        }

        rx.await.unwrap_or_default()
    }

    pub async fn credit_position(
        &self,
        account_id: AccountId,
        instrument: InstrumentId,
        quantity: Quantity,
    ) -> Vec<Event> {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(AsyncEngineCommand::CreditPosition {
                account_id,
                instrument,
                quantity,
                respond_to: tx,
            })
            .await
            .is_err()
        {
            return Vec::new();
        }

        rx.await.unwrap_or_default()
    }

    pub async fn snapshot(&self, instrument: InstrumentId) -> Option<BookSnapshot> {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(AsyncEngineCommand::Snapshot {
                instrument,
                respond_to: tx,
            })
            .await
            .is_err()
        {
            return None;
        }

        rx.await.ok()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }
}

pub fn spawn_engine(command_buffer: usize, event_buffer: usize) -> EngineHandle {
    let (command_tx, mut command_rx) = mpsc::channel(command_buffer);
    let (event_tx, _) = broadcast::channel(event_buffer);

    let event_publisher = event_tx.clone();
    tokio::spawn(async move {
        let mut engine = MatchingEngine::new();

        while let Some(command) = command_rx.recv().await {
            match command {
                AsyncEngineCommand::Submit { order, respond_to } => {
                    let events = engine.apply(EngineCommand::Submit(order));
                    publish_events(&event_publisher, &events);
                    let _ = respond_to.send(events);
                }
                AsyncEngineCommand::Cancel {
                    order_id,
                    respond_to,
                } => {
                    let events = engine.apply(EngineCommand::Cancel { order_id });
                    publish_events(&event_publisher, &events);
                    let _ = respond_to.send(events);
                }
                AsyncEngineCommand::Replace {
                    replace,
                    respond_to,
                } => {
                    let events = engine.apply(EngineCommand::Replace(replace));
                    publish_events(&event_publisher, &events);
                    let _ = respond_to.send(events);
                }
                AsyncEngineCommand::CreditCash {
                    account_id,
                    amount,
                    respond_to,
                } => {
                    let events = engine.apply(EngineCommand::CreditCash { account_id, amount });
                    publish_events(&event_publisher, &events);
                    let _ = respond_to.send(events);
                }
                AsyncEngineCommand::CreditPosition {
                    account_id,
                    instrument,
                    quantity,
                    respond_to,
                } => {
                    let events = engine.apply(EngineCommand::CreditPosition {
                        account_id,
                        instrument,
                        quantity,
                    });
                    publish_events(&event_publisher, &events);
                    let _ = respond_to.send(events);
                }
                AsyncEngineCommand::Snapshot {
                    instrument,
                    respond_to,
                } => {
                    let _ = respond_to.send(engine.snapshot(instrument));
                }
            }
        }
    });

    EngineHandle {
        tx: command_tx,
        events: event_tx,
    }
}

fn publish_events(publisher: &broadcast::Sender<Event>, events: &[Event]) {
    for &event in events {
        let _ = publisher.send(event);
    }
}
