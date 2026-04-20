# Trading Engine

A small Rust trading engine with:

- Limit order book
- Price-time priority matching
- Market and limit orders
- Partial fills
- Cancellations and replace/reprice
- Multi-instrument routing
- Account cash / position checks
- Tokio-based command processing
- TCP JSON-line gateway
- Snapshot + command-log recovery

## Run

```bash
cargo run
```

The example process starts a TCP gateway on `127.0.0.1:7001`.

## Test

```bash
cargo test
```

## Benchmark Build

```bash
cargo bench --no-run
```

## Gateway Requests

Send one JSON object per line. Supported request types:

- `submit`
- `cancel`
- `replace`
- `credit_cash`
- `credit_position`
- `snapshot`

Example:

```json
{"type":"snapshot","instrument":1}
```

