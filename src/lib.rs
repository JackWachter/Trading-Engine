pub mod book;
pub mod engine;
pub mod events;
pub mod gateway;
pub mod persistence;
pub mod risk;
pub mod types;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::book::OrderBook;
    use crate::engine::{spawn_engine, MatchingEngine};
    use crate::events::{Event, RejectReason};
    use crate::persistence::PersistentMatchingEngine;
    use crate::types::{EngineCommand, NewOrder, ReplaceOrder, Side};

    const BTC_USD: u64 = 1;
    const ETH_USD: u64 = 2;
    const MAKER: u64 = 10;
    const TAKER: u64 = 20;

    #[test]
    fn limit_order_rests_on_empty_book() {
        let mut book = OrderBook::new(BTC_USD);
        let events = book.submit(NewOrder::limit(1, BTC_USD, Side::Bid, 100, 10));

        assert!(events.contains(&Event::Accepted {
            instrument: BTC_USD,
            order_id: 1,
            sequence: 0,
        }));
        assert!(events.contains(&Event::Rested {
            instrument: BTC_USD,
            order_id: 1,
            side: Side::Bid,
            price: 100,
            remaining: 10,
            sequence: 0,
        }));
    }

    #[test]
    fn price_time_priority_is_fifo_within_level() {
        let mut book = OrderBook::new(BTC_USD);
        book.submit(NewOrder::limit(1, BTC_USD, Side::Ask, 101, 5));
        book.submit(NewOrder::limit(2, BTC_USD, Side::Ask, 101, 5));

        let events = book.submit(NewOrder::limit(3, BTC_USD, Side::Bid, 101, 7));
        let trades: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::Trade {
                    maker_order_id,
                    quantity,
                    ..
                } => Some((*maker_order_id, *quantity)),
                _ => None,
            })
            .collect();

        assert_eq!(trades, vec![(1, 5), (2, 2)]);
    }

    #[test]
    fn risk_rejects_buy_without_cash() {
        let mut engine = MatchingEngine::new();
        let events = engine.apply(EngineCommand::Submit(
            NewOrder::limit(1, BTC_USD, Side::Bid, 100, 10).with_account(TAKER),
        ));

        assert_eq!(
            events,
            vec![Event::Rejected {
                instrument: Some(BTC_USD),
                order_id: 1,
                reason: RejectReason::InsufficientCash,
                sequence: 0,
            }]
        );
    }

    #[test]
    fn engine_matches_and_updates_balances() {
        let mut engine = MatchingEngine::new();
        engine.apply(EngineCommand::CreditCash {
            account_id: TAKER,
            amount: 10_000,
        });
        engine.apply(EngineCommand::CreditPosition {
            account_id: MAKER,
            instrument: BTC_USD,
            quantity: 10,
        });

        engine.apply(EngineCommand::Submit(
            NewOrder::limit(1, BTC_USD, Side::Ask, 101, 5).with_account(MAKER),
        ));
        let events = engine.apply(EngineCommand::Submit(
            NewOrder::market(2, BTC_USD, Side::Bid, 5).with_account(TAKER),
        ));

        assert!(events.iter().any(|event| matches!(
            event,
            Event::Trade {
                instrument,
                maker_order_id: 1,
                taker_order_id: 2,
                price: 101,
                quantity: 5,
                ..
            } if *instrument == BTC_USD
        )));
        assert!(engine.snapshot(BTC_USD).best_ask.is_none());
    }

    #[test]
    fn matching_engine_routes_independent_books_by_instrument() {
        let mut engine = MatchingEngine::new();
        engine.apply(EngineCommand::CreditCash {
            account_id: MAKER,
            amount: 100_000,
        });
        engine.apply(EngineCommand::CreditPosition {
            account_id: TAKER,
            instrument: ETH_USD,
            quantity: 5,
        });

        engine.apply(EngineCommand::Submit(
            NewOrder::limit(1, BTC_USD, Side::Bid, 100, 10).with_account(MAKER),
        ));
        engine.apply(EngineCommand::Submit(
            NewOrder::limit(2, ETH_USD, Side::Ask, 200, 5).with_account(TAKER),
        ));

        let btc = engine.snapshot(BTC_USD);
        let eth = engine.snapshot(ETH_USD);

        assert_eq!(btc.best_bid.map(|level| level.price), Some(100));
        assert_eq!(eth.best_ask.map(|level| level.price), Some(200));
    }

    #[test]
    fn replace_reprices_order_and_requeues_priority() {
        let mut engine = MatchingEngine::new();
        engine.apply(EngineCommand::CreditPosition {
            account_id: MAKER,
            instrument: BTC_USD,
            quantity: 10,
        });
        engine.apply(EngineCommand::CreditCash {
            account_id: TAKER,
            amount: 10_000,
        });

        engine.apply(EngineCommand::Submit(
            NewOrder::limit(1, BTC_USD, Side::Ask, 101, 5).with_account(MAKER),
        ));
        engine.apply(EngineCommand::Submit(
            NewOrder::limit(2, BTC_USD, Side::Ask, 101, 5).with_account(MAKER),
        ));

        let replace_events = engine.apply(EngineCommand::Replace(ReplaceOrder {
            order_id: 1,
            new_price: 100,
            new_quantity: 4,
        }));
        assert!(replace_events.iter().any(|event| matches!(
            event,
            Event::Replaced {
                instrument,
                order_id,
                old_price,
                new_price,
                ..
            } if *instrument == BTC_USD && *order_id == 1 && *old_price == 101 && *new_price == 100
        )));

        let trade_events = engine.apply(EngineCommand::Submit(
            NewOrder::limit(3, BTC_USD, Side::Bid, 101, 6).with_account(TAKER),
        ));
        let trades: Vec<_> = trade_events
            .iter()
            .filter_map(|event| match event {
                Event::Trade {
                    maker_order_id,
                    quantity,
                    ..
                } => Some((*maker_order_id, *quantity)),
                _ => None,
            })
            .collect();

        assert_eq!(trades, vec![(1, 4), (2, 2)]);
    }

    #[test]
    fn persistence_recovers_live_orders() {
        let root = temp_store_path("engine-recovery");
        let _ = fs::remove_dir_all(&root);
        let mut persistent = PersistentMatchingEngine::new(&root).expect("store should initialize");

        persistent
            .apply(EngineCommand::CreditCash {
                account_id: TAKER,
                amount: 10_000,
            })
            .expect("credit should persist");
        persistent
            .apply(EngineCommand::CreditPosition {
                account_id: MAKER,
                instrument: BTC_USD,
                quantity: 10,
            })
            .expect("position should persist");
        persistent
            .apply(EngineCommand::Submit(
                NewOrder::limit(1, BTC_USD, Side::Ask, 101, 5).with_account(MAKER),
            ))
            .expect("submit should persist");
        persistent.snapshot().expect("snapshot should persist");

        let recovered = PersistentMatchingEngine::new(&root).expect("store should recover");
        assert_eq!(
            recovered
                .engine()
                .snapshot(BTC_USD)
                .best_ask
                .map(|level| level.price),
            Some(101)
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tokio_engine_processes_commands_sequentially() {
        let engine = spawn_engine(128, 128);

        engine.credit_cash(TAKER, 10_000).await;
        engine.credit_position(MAKER, BTC_USD, 10).await;

        let sell =
            engine.submit(NewOrder::limit(1, BTC_USD, Side::Ask, 100, 10).with_account(MAKER));
        let buy = engine.submit(NewOrder::limit(2, BTC_USD, Side::Bid, 100, 6).with_account(TAKER));

        let (sell_events, buy_events) = tokio::join!(sell, buy);

        assert!(!sell_events.is_empty());
        assert!(buy_events.iter().any(|event| matches!(
            event,
            Event::Trade {
                instrument: BTC_USD,
                maker_order_id: 1,
                taker_order_id: 2,
                quantity: 6,
                ..
            }
        )));

        let snapshot = engine
            .snapshot(BTC_USD)
            .await
            .expect("engine should be alive");
        assert_eq!(snapshot.best_ask.map(|level| level.total_qty), Some(4));
    }

    fn temp_store_path(prefix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough for tests")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{nanos}"))
    }
}
