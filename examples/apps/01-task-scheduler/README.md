# 01 — Distributed task scheduler

A production-shaped task scheduler with:

- **Worker pool** with N async workers competing for tasks off a priority queue
- **DAG dependencies** — a task only runs once every upstream dependency completes
- **Retry/backoff** with exponential delay, max attempt cap, and jitter
- **Persistent state** via SQLite (idempotent enqueue, durable status)
- **HTTP control plane** (FastAPI) for submitting tasks and querying status
- **Structured event log** modelled as a sealed-union stream
- **Graceful shutdown** that drains in-flight tasks

## Files

| File | Responsibility |
|---|---|
| `src/domain/ids.ty` | `newtype` wrappers for the ID kinds in the system |
| `src/domain/models.ty` | `model` types at the HTTP boundary, internal `class` types |
| `src/runtime/config.ty` | `freeze let` + `comptime let` for runtime constants |
| `src/storage/store.ty` | SQLite-backed durable storage with `Result[T, E]` returns |
| `src/runtime/scheduler.ty` | DAG resolution + ready-queue management |
| `src/runtime/worker.ty` | Async worker loop with retry/backoff |
| `src/runtime/handlers.ty` | Sample task handlers registered with the scheduler |
| `src/transport/api.ty` | FastAPI control-plane endpoints |
| `src/main.ty` | Wires everything together, owns the lifecycle |

## Features exercised

- `newtype TaskId = int`, `newtype RunId = int`, `newtype JobName = str`
- `freeze let` config (port, worker count, retry policy)
- `comptime let` for build-time constants
- Sealed-union event stream + exhaustive `match`
- `Result[T, StoreError]` everywhere persistence happens
- `with`-chained `?` for multi-step transactions
- `go worker_loop(...)` for spawning workers via the runtime registry
- `gather:` over independent startup tasks
- `impl` blocks for `Scheduler`, `Worker`, `Store`
- `pub` markers for module exports
- Pattern-matched state machines for task lifecycle

## Running

```bash
cd examples/apps/01-task-scheduler
tyc check src/
tyc build
python build/main.py
# in another shell:
curl -X POST localhost:8000/tasks \
     -H 'content-type: application/json' \
     -d '{"job":"hello","args":{"name":"world"},"priority":5}'
curl localhost:8000/tasks/1
curl localhost:8000/events?since=0
```
