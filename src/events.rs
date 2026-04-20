use serde::{Deserialize, Serialize};

use crate::types::{AccountId, Cash, InstrumentId, OrderId, Price, Quantity, Sequence, Side};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    DuplicateOrderId,
    InvalidQuantity,
    MissingPrice,
    PriceNotAllowedForMarket,
    UnknownOrderId,
    ReplaceOnlySupportedForRestingLimitOrders,
    InsufficientCash,
    InsufficientPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    Accepted {
        instrument: InstrumentId,
        order_id: OrderId,
        sequence: Sequence,
    },
    Rejected {
        instrument: Option<InstrumentId>,
        order_id: OrderId,
        reason: RejectReason,
        sequence: Sequence,
    },
    Trade {
        instrument: InstrumentId,
        maker_order_id: OrderId,
        taker_order_id: OrderId,
        side: Side,
        price: Price,
        quantity: Quantity,
        maker_remaining: Quantity,
        taker_remaining: Quantity,
        sequence: Sequence,
    },
    Rested {
        instrument: InstrumentId,
        order_id: OrderId,
        side: Side,
        price: Price,
        remaining: Quantity,
        sequence: Sequence,
    },
    Filled {
        instrument: InstrumentId,
        order_id: OrderId,
        sequence: Sequence,
    },
    Cancelled {
        instrument: InstrumentId,
        order_id: OrderId,
        cancelled_qty: Quantity,
        sequence: Sequence,
    },
    Replaced {
        instrument: InstrumentId,
        order_id: OrderId,
        old_price: Price,
        new_price: Price,
        old_remaining: Quantity,
        new_quantity: Quantity,
        sequence: Sequence,
    },
    LevelUpdated {
        instrument: InstrumentId,
        side: Side,
        price: Price,
        total_qty: Quantity,
        order_count: usize,
        sequence: Sequence,
    },
    BestBidAskUpdated {
        instrument: InstrumentId,
        best_bid: Option<Price>,
        best_ask: Option<Price>,
        sequence: Sequence,
    },
    CashCredited {
        account_id: AccountId,
        amount: Cash,
        new_cash_balance: Cash,
        sequence: Sequence,
    },
    PositionCredited {
        account_id: AccountId,
        instrument: InstrumentId,
        quantity: Quantity,
        new_position: Quantity,
        sequence: Sequence,
    },
}
