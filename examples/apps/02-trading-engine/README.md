# 02 — Limit-order matching engine

A self-contained price-time-priority matching engine. No external
services — everything in-process so the focus stays on Typhon's
type system, sealed unions, and pattern matching.

- **Limit-order book** (bids + asks, sorted by price-time priority)
- **Matching engine** that produces fills and book-state diffs
- **Risk gate** with per-symbol position and notional limits
- **Trader accounts** with simple cash + position accounting
- **Market-data fan-out** modelled as a sealed-union event stream
- **Backtest runner** that replays a synthetic order tape

## Files

| File | Responsibility |
|---|---|
| `src/ids.ty` | `newtype` wrappers — `Symbol`, `OrderId`, `TraderId` |
| `src/money.ty` | `newtype Price = int`, `newtype Qty = int`, `newtype Notional = int` (all stored as integer ticks/cents) + arithmetic helpers |
| `src/orders.ty` | Sealed-union order types, status, and side enums |
| `src/book.ty` | Order-book data structure with `impl`-attached methods |
| `src/risk.ty` | Pre-trade risk gates (`Result`-returning) |
| `src/engine.ty` | Matching engine producing fills + sealed-union market events |
| `src/portfolio.ty` | Trader accounts, cash + position bookkeeping |
| `src/feed.ty` | Synthetic order-tape generator for backtests |
| `src/main.ty` | CLI entry that runs a backtest and prints a summary |

## Features exercised

- `newtype` for **every** ID and unit (Price, Qty, Notional, Symbol, OrderId, TraderId)
  — wrong-axis bugs are unrepresentable
- `frozen class` for value types — Order, Fill, MarketEvent
- `pub` markers throughout for an explicit public surface
- `freeze let` venue configuration (tick size, lot size, fees)
- `comptime let` for build-time constants (currency code, version)
- Sealed-union order types + exhaustive `match`
- `Result[T, RiskError]` from the risk gate
- `with`-chained `?` for `submit → risk-check → match → settle` flow
- `interface` for the market-event sink so the engine doesn't depend on the printer

## Running

```bash
cd examples/apps/02-trading-engine
tyc check src/
tyc build
python build/main.py
```

The backtest writes a one-line summary per fill plus an aggregate P&L
table at the end.
