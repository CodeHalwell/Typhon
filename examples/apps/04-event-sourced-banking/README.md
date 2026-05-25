# 04 — Event-sourced double-entry banking

A minimal banking core organised around **commands**, **events**, and
**projections**. Every state change is an immutable event in the log;
the current balance, ledger, and customer profile are projected by
folding the event log forward. Multi-currency with a frozen FX rate
card, transactional transfers, and a tiny AML rule pipeline.

- **Commands** — what the user *intends* (OpenAccount, Deposit, Transfer, …)
- **Events** — what *actually happened* (AccountOpened, MoneyDeposited, …)
- **Aggregates** — `Account` is reconstituted from its event slice
- **Projections** — `LedgerProjection`, `BalanceProjection`, `AuditProjection`
- **AML pipeline** — composable rules returning `Result[(), Flag]`
- **FX** — `freeze let` rate card with `newtype Currency`
- **Snapshots** — periodic snapshots reduce replay cost

## Files

| File | Responsibility |
|---|---|
| `src/domain/ids.ty` | `newtype` IDs: `AccountId`, `CustomerId`, `TransactionId`, `Currency` |
| `src/domain/money.ty` | `newtype Money = int` (minor units) + FX rate card |
| `src/domain/events.ty` | Sealed-union event log + envelope |
| `src/domain/commands.ty` | Sealed-union of accepted commands |
| `src/aggregate/aggregate.ty` | `Account` rebuilt by folding events |
| `src/application/projections.ty` | Balance + ledger + audit projections |
| `src/aggregate/aml.ty` | Composable AML rules over `Result` |
| `src/application/handlers.ty` | Command handlers: command → event(s) (or rejection) |
| `src/storage/store.ty` | In-memory append-only event store + snapshots |
| `src/main.ty` | Worked sample: open accounts → fund → transfer → query |

## Features exercised

- Sealed unions for **commands**, **events**, **projections** — every fold/match exhaustive
- `newtype` for all monetary types and IDs
- `freeze let` for the FX rate card (deep-immutable: nested currencies can't be swapped at runtime)
- `comptime let` for the base currency
- `Result[Event, RejectionReason]` from every handler
- `with`-chained `?` in the transfer handler (debit + credit must both succeed)
- `pub` markers on the public command/event surface
- `frozen class` events — every event is immutable

## Running

```bash
cd examples/apps/04-event-sourced-banking
tyc check src/
tyc build
python build/main.py
```
