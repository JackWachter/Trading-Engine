use criterion::{criterion_group, criterion_main, Criterion};
use trading_engine::engine::MatchingEngine;
use trading_engine::types::{EngineCommand, NewOrder, Side};

const BTC_USD: u64 = 1;
const MAKER: u64 = 10;
const TAKER: u64 = 20;

fn matching_benchmark(c: &mut Criterion) {
    c.bench_function("submit_crossing_limit_order", |b| {
        b.iter(|| {
            let mut engine = MatchingEngine::new();
            let _ = engine.apply(EngineCommand::CreditCash {
                account_id: TAKER,
                amount: 1_000_000,
            });
            let _ = engine.apply(EngineCommand::CreditPosition {
                account_id: MAKER,
                instrument: BTC_USD,
                quantity: 10_000,
            });
            let _ = engine.apply(EngineCommand::Submit(
                NewOrder::limit(1, BTC_USD, Side::Ask, 101, 100).with_account(MAKER),
            ));
            let _ = engine.apply(EngineCommand::Submit(
                NewOrder::limit(2, BTC_USD, Side::Bid, 101, 100).with_account(TAKER),
            ));
        });
    });
}

criterion_group!(benches, matching_benchmark);
criterion_main!(benches);
