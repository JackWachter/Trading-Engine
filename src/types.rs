use serde::{Deserialize, Serialize};

pub type OrderId = u64;
pub type AccountId = u64;
pub type InstrumentId = u64;
pub type Price = u64;
pub type Quantity = u64;
pub type Sequence = u64;
pub type Cash = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Bid,
    Ask,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    Market,
    Limit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    Gtc,
    Ioc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewOrder {
    pub id: OrderId,
    pub account: AccountId,
    pub instrument: InstrumentId,
    pub side: Side,
    pub order_type: OrderType,
    pub price: Option<Price>,
    pub quantity: Quantity,
    pub time_in_force: TimeInForce,
}

impl NewOrder {
    pub fn limit(
        id: OrderId,
        instrument: InstrumentId,
        side: Side,
        price: Price,
        quantity: Quantity,
    ) -> Self {
        Self {
            id,
            account: 0,
            instrument,
            side,
            order_type: OrderType::Limit,
            price: Some(price),
            quantity,
            time_in_force: TimeInForce::Gtc,
        }
    }

    pub fn market(id: OrderId, instrument: InstrumentId, side: Side, quantity: Quantity) -> Self {
        Self {
            id,
            account: 0,
            instrument,
            side,
            order_type: OrderType::Market,
            price: None,
            quantity,
            time_in_force: TimeInForce::Ioc,
        }
    }

    pub fn with_account(mut self, account: AccountId) -> Self {
        self.account = account;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceOrder {
    pub order_id: OrderId,
    pub new_price: Price,
    pub new_quantity: Quantity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineCommand {
    Submit(NewOrder),
    Cancel {
        order_id: OrderId,
    },
    Replace(ReplaceOrder),
    CreditCash {
        account_id: AccountId,
        amount: Cash,
    },
    CreditPosition {
        account_id: AccountId,
        instrument: InstrumentId,
        quantity: Quantity,
    },
}
