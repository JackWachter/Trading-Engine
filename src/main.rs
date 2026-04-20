use trading_engine::engine::spawn_engine;
use trading_engine::gateway::serve_tcp;
use trading_engine::types::{NewOrder, ReplaceOrder, Side};

const BTC_USD: u64 = 1;
const MAKER: u64 = 10;
const TAKER: u64 = 20;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let engine = spawn_engine(1024, 1024);

    for event in engine.credit_position(MAKER, BTC_USD, 20).await {
        println!("{event:?}");
    }
    for event in engine.credit_cash(TAKER, 100_000).await {
        println!("{event:?}");
    }

    for event in engine
        .submit(NewOrder::limit(1, BTC_USD, Side::Ask, 101, 12).with_account(MAKER))
        .await
    {
        println!("{event:?}");
    }

    for event in engine
        .submit(NewOrder::limit(2, BTC_USD, Side::Ask, 102, 8).with_account(MAKER))
        .await
    {
        println!("{event:?}");
    }

    for event in engine
        .replace(ReplaceOrder {
            order_id: 2,
            new_price: 100,
            new_quantity: 6,
        })
        .await
    {
        println!("{event:?}");
    }

    for event in engine
        .submit(NewOrder::limit(3, BTC_USD, Side::Bid, 102, 15).with_account(TAKER))
        .await
    {
        println!("{event:?}");
    }

    println!("snapshot: {:?}", engine.snapshot(BTC_USD).await);

    let gateway_engine = engine.clone();
    tokio::spawn(async move {
        let _ = serve_tcp("127.0.0.1:7001", gateway_engine).await;
    });

    let _ = tokio::signal::ctrl_c().await;
}
