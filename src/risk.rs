use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::book::BookSnapshot;
use crate::events::RejectReason;
use crate::types::{AccountId, Cash, InstrumentId, NewOrder, Price, Quantity, Side};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PositionState {
    pub available: Quantity,
    pub reserved: Quantity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AccountState {
    pub cash: Cash,
    pub reserved_cash: Cash,
    pub positions: HashMap<InstrumentId, PositionState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reservation {
    BuyCash {
        limit_price: Price,
        reserved_cash: Cash,
    },
    SellPosition {
        reserved_qty: Quantity,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservedOrder {
    pub account_id: AccountId,
    pub reservation: Reservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AccountManager {
    accounts: HashMap<AccountId, AccountState>,
}

impl AccountManager {
    pub fn credit_cash(&mut self, account_id: AccountId, amount: Cash) -> Cash {
        let account = self.accounts.entry(account_id).or_default();
        account.cash += amount;
        account.cash
    }

    pub fn credit_position(
        &mut self,
        account_id: AccountId,
        instrument: InstrumentId,
        quantity: Quantity,
    ) -> Quantity {
        let account = self.accounts.entry(account_id).or_default();
        let position = account.positions.entry(instrument).or_default();
        position.available += quantity;
        position.available
    }

    pub fn state(&self) -> &HashMap<AccountId, AccountState> {
        &self.accounts
    }

    pub fn from_state(accounts: HashMap<AccountId, AccountState>) -> Self {
        Self { accounts }
    }

    pub fn reserve_for_order(
        &mut self,
        order: &NewOrder,
        snapshot: BookSnapshot,
    ) -> Result<ReservedOrder, RejectReason> {
        let account = self.accounts.entry(order.account).or_default();
        let reservation = match order.side {
            Side::Bid => {
                let limit_price = order
                    .price
                    .unwrap_or_else(|| estimate_market_buy_price(snapshot, order.quantity));
                let required_cash = limit_price.saturating_mul(order.quantity);
                if account.cash < required_cash {
                    return Err(RejectReason::InsufficientCash);
                }
                account.cash -= required_cash;
                account.reserved_cash += required_cash;
                Reservation::BuyCash {
                    limit_price,
                    reserved_cash: required_cash,
                }
            }
            Side::Ask => {
                let position = account.positions.entry(order.instrument).or_default();
                if position.available < order.quantity {
                    return Err(RejectReason::InsufficientPosition);
                }
                position.available -= order.quantity;
                position.reserved += order.quantity;
                Reservation::SellPosition {
                    reserved_qty: order.quantity,
                }
            }
        };

        Ok(ReservedOrder {
            account_id: order.account,
            reservation,
        })
    }

    pub fn release_remaining(
        &mut self,
        account_id: AccountId,
        instrument: InstrumentId,
        reservation: &mut Reservation,
    ) {
        let account = self.accounts.entry(account_id).or_default();
        match reservation {
            Reservation::BuyCash { reserved_cash, .. } => {
                account.reserved_cash = account.reserved_cash.saturating_sub(*reserved_cash);
                account.cash += *reserved_cash;
                *reserved_cash = 0;
            }
            Reservation::SellPosition { reserved_qty } => {
                let position = account.positions.entry(instrument).or_default();
                position.reserved = position.reserved.saturating_sub(*reserved_qty);
                position.available += *reserved_qty;
                *reserved_qty = 0;
            }
        }
    }

    pub fn apply_trade(
        &mut self,
        account_id: AccountId,
        instrument: InstrumentId,
        side: Side,
        price: Price,
        quantity: Quantity,
        reservation: &mut Reservation,
    ) {
        let account = self.accounts.entry(account_id).or_default();
        let notional = price.saturating_mul(quantity);

        match (side, reservation) {
            (
                Side::Bid,
                Reservation::BuyCash {
                    limit_price,
                    reserved_cash,
                },
            ) => {
                let reserved_for_fill = limit_price.saturating_mul(quantity);
                account.reserved_cash = account.reserved_cash.saturating_sub(reserved_for_fill);
                account.cash += reserved_for_fill.saturating_sub(notional);
                *reserved_cash = reserved_cash.saturating_sub(reserved_for_fill);
                let position = account.positions.entry(instrument).or_default();
                position.available += quantity;
            }
            (Side::Ask, Reservation::SellPosition { reserved_qty }) => {
                let position = account.positions.entry(instrument).or_default();
                position.reserved = position.reserved.saturating_sub(quantity);
                *reserved_qty = reserved_qty.saturating_sub(quantity);
                account.cash += notional;
            }
            _ => {}
        }
    }
}

fn estimate_market_buy_price(snapshot: BookSnapshot, quantity: Quantity) -> Price {
    let reference = snapshot
        .best_ask
        .map(|ask| ask.price)
        .or(snapshot.best_bid.map(|bid| bid.price))
        .unwrap_or(0);
    let _ = quantity;
    reference.saturating_mul(2)
}
