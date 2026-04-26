# Trading Engine

A Rust-based electronic trading engine implementing a price-time-priority limit order book, deterministic matching core, account-level risk checks, asynchronous command processing, TCP JSON-lines gateway, and snapshot/command-log recovery.

This project is intended as a systems-level implementation of core exchange mechanics: order entry, matching, cancellations, replaces, market/limit order behavior, account reservation, event generation, and state recovery.

## Features

- Price-time-priority limit order book
- Market and limit orders
- Good-til-cancelled and immediate-or-cancel behavior
- Partial fills
- Order cancellation
- Replace/reprice support
- Multi-instrument routing
- Account cash and position checks
- Deterministic event generation
- Top-of-book and level update events
- Tokio-based asynchronous command interface
- TCP JSON-lines gateway
- Snapshot and command-log recovery
- Criterion benchmark for the synchronous matching-core path

## Architecture

The system separates the deterministic matching core from the asynchronous gateway and event distribution layer.

```text
TCP JSON Client(s)
        |
        v
 JSON-lines Gateway
        |
        v
 Tokio command channel
        |
        v
 Single-owner MatchingEngine task
        |
        +--> OrderBook per instrument
        |       +--> BTreeMap price levels
        |       +--> Slab-backed resting orders
        |       +--> FIFO linked list per price level
        |       +--> HashMap order index
        |
        +--> AccountManager risk checks
        |
        +--> Event stream / broadcast channel
        |
        +--> Optional command log + snapshot recovery
```

The mutable matching engine state is owned by a single task. External callers submit commands through Tokio channels and receive responses through one-shot replies. Market events are distributed through a broadcast channel. This avoids shared mutable access to the order book while still allowing concurrent clients to interact with the engine.

## Core Data Structures

The order book is designed around deterministic price-time priority.

### Price levels

Each instrument has independent bid and ask books:

```rust
bids: BTreeMap<Price, PriceLevel>
asks: BTreeMap<Price, PriceLevel>
```

`BTreeMap` keeps price levels sorted so the engine can efficiently access the best bid and best ask.

### Resting orders

Resting orders are stored in a slab:

```rust
orders: Slab<OrderNode>
```

Each `OrderNode` stores order metadata, remaining quantity, side, price, sequence, and intrusive `prev` / `next` pointers.

### FIFO within a price level

Each `PriceLevel` stores:

```rust
head: Option<usize>
tail: Option<usize>
total_qty: Quantity
order_count: usize
```

FIFO priority within a price level is maintained with a doubly linked list over slab indices. New resting orders append to the tail. Matching consumes from the head.

### Order lookup and cancellation

The engine maintains an external order ID to slab index map:

```rust
order_index: HashMap<OrderId, usize>
```

This allows O(1)-style lookup for cancellation and order state access, subject to normal hash map behavior.

## Matching Semantics

The engine supports:

* Buy and sell orders
* Market orders
* Limit orders
* Partial fills
* Resting limit orders
* Immediate cancellation of unfilled market / IOC quantity
* FIFO priority at the same price level
* Best bid / best ask updates
* Level quantity updates
* Duplicate order ID rejection
* Invalid quantity rejection
* Missing price rejection for limit orders
* Price rejection for market orders with explicit prices

A crossing limit order matches against the best available opposite-side price while the order remains marketable. Resting maker orders determine the execution price.

## Risk and Account Checks

Orders are checked against account state before entering the book.

Buy orders reserve cash based on limit price or an estimated market-buy reference price.

Sell orders reserve position quantity before entering the book.

The account manager tracks:

* Available cash
* Reserved cash
* Available position quantity
* Reserved position quantity

When trades execute, reserved balances are released or converted into filled cash/position state.

## Async Command Model

The asynchronous interface wraps the deterministic matching core in an actor-style command processor.

Supported async commands:

* `Submit`
* `Cancel`
* `Replace`
* `CreditCash`
* `CreditPosition`
* `Snapshot`

Callers send commands through an `mpsc` channel. The engine replies through `oneshot` channels. Events are published through a Tokio `broadcast` channel.

The matching engine itself is processed sequentially by one owner task, even when the Tokio runtime is multi-threaded. This keeps book mutation deterministic and avoids shared mutable order-book state across worker threads.

## TCP JSON-lines Gateway

Running the binary starts a TCP gateway on:

```text
127.0.0.1:7001
```

Each request is one JSON object followed by a newline.

### Run

```bash
cargo run
```

### Example: snapshot

```bash
printf '{"type":"snapshot","instrument":1}\n' | nc 127.0.0.1 7001
```

### Example: credit cash

```bash
printf '{"type":"credit_cash","account_id":20,"amount":1000000}\n' | nc 127.0.0.1 7001
```

### Example: credit position

```bash
printf '{"type":"credit_position","account_id":10,"instrument":1,"quantity":10000}\n' | nc 127.0.0.1 7001
```

### Example: submit limit order

```bash
printf '{"type":"submit","order":{"id":1,"account":10,"instrument":1,"side":"Ask","order_type":"Limit","price":101,"quantity":100,"time_in_force":"Gtc"}}\n' | nc 127.0.0.1 7001
```

### Example: crossing order

```bash
printf '{"type":"submit","order":{"id":2,"account":20,"instrument":1,"side":"Bid","order_type":"Limit","price":101,"quantity":100,"time_in_force":"Gtc"}}\n' | nc 127.0.0.1 7001
```

### Example: cancel

```bash
printf '{"type":"cancel","order_id":1}\n' | nc 127.0.0.1 7001
```

### Example: replace

```bash
printf '{"type":"replace","replace":{"order_id":1,"new_price":100,"new_quantity":50}}\n' | nc 127.0.0.1 7001
```

## Event Model

The engine emits structured events for all state transitions.

Event types include:

* `Accepted`
* `Rejected`
* `Trade`
* `Rested`
* `Filled`
* `Cancelled`
* `Replaced`
* `LevelUpdated`
* `BestBidAskUpdated`
* `CashCredited`
* `PositionCredited`

These events make the matching path auditable and allow downstream consumers to reconstruct book/account behavior from emitted state transitions.

## Persistence and Recovery

The project includes a persistent matching-engine wrapper with:

* Append-only command log
* Snapshot file
* Replay from latest snapshot plus subsequent log entries

This allows live resting orders and account state to be recovered after restart.

Persistence is intentionally separate from the core benchmark path. The benchmark described below measures the synchronous in-memory matching path, not command-log persistence or TCP gateway overhead.

## Testing

Run the test suite:

```bash
cargo test
```

The tests cover:

* Resting limit orders
* FIFO price-time priority
* Risk rejection for insufficient cash
* Matching and account balance updates
* Multi-instrument routing
* Replace/reprice behavior
* Snapshot/command-log recovery
* Sequential engine behavior under a multi-threaded Tokio runtime

## Benchmarking

The repository includes a Criterion benchmark for the synchronous matching-core path:

```bash
cargo bench
```

Benchmark target:

```text
submit_crossing_limit_order
```

The benchmark creates a fresh `MatchingEngine` and executes four commands per iteration:

1. Credit taker cash
2. Credit maker position
3. Submit maker ask
4. Submit crossing taker bid

Representative result from a local run:

```text
submit_crossing_limit_order: ~868 ns to ~877 ns per iteration
midpoint: ~872 ns per 4-command scenario
throughput: ~1.15M benchmark iterations/sec
```

### Benchmark interpretation

This is a synchronous in-memory microbenchmark of `MatchingEngine::apply`.

It measures the deterministic matching-core path for a small crossing-order scenario. It does **not** include:

* TCP gateway latency
* JSON serialization/deserialization
* Network I/O
* Async channel send/receive overhead
* Multi-client gateway contention
* Persistence / command-log writes
* Market-data fanout overhead
* Kernel bypass
* Real exchange connectivity

The benchmark should be interpreted as a lower-level matching-core measurement, not as an end-to-end trading gateway latency number.

### Benchmark environment

```text
CPU: Intel Ultra 9
OS: Windows 11
Rust version: Rust 1.83
Build command: cargo bench
Criterion version: 0.3.6
Benchmark mode: synchronous in-memory microbenchmark
Persistence enabled: no
Network I/O included: no
Async gateway included: no
```

## Design Tradeoffs

### Single-owner engine state

The matching engine mutates book state sequentially inside one owner task. This simplifies correctness, preserves deterministic event ordering, and avoids shared mutable book state across threads.

### `BTreeMap` price levels

`BTreeMap` gives straightforward sorted price-level access and clean best-bid/best-ask behavior. A production-grade ultra-low-latency implementation might consider flatter price-level structures, arena allocation, preallocated price ladders, or cache-specialized layouts depending on instrument type and tick space.

### Slab-backed orders

Resting orders are stored in a slab to provide stable indices for linked-list pointers and order lookup. This avoids object ownership complexity while supporting efficient insertion, cancellation, and FIFO traversal.

### Intrusive FIFO queues

Each price level maintains head/tail indices into the slab. This makes FIFO matching and cancellation straightforward without allocating a separate linked-list node object per order.

### Gateway separated from matching core

The TCP JSON-lines gateway is intentionally outside the benchmarked matching core. This keeps the matching logic testable and allows gateway, serialization, persistence, and networking overhead to be measured separately.

## Current Limitations

This is an educational/research trading engine, not a production exchange or broker system.

Known limitations:

* No real exchange connectivity
* No FIX, OUCH, or ITCH protocol support
* No binary market-data feed
* No kernel-bypass networking
* No persistent benchmark of end-to-end gateway latency
* No p50/p95/p99 latency histogram yet
* No hardware-pinned benchmark setup yet
* No preallocated arena for all order-book structures
* No realistic market-data replay harness yet
* No authentication or production security model for the TCP gateway
