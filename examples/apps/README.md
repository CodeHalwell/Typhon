# Typhon — large production-shaped apps

This directory holds **fifteen** multi-file projects deliberately built to
stress the language end-to-end: every app touches a wide cross-section of
Typhon features (sealed unions, `Result[T, E]`, `?`, `with`-chains,
`impl`/`extend`, `model`, `frozen class`, `freeze let`, `newtype`,
`comptime let`, `lazy import`, `gather:`, `go`, `pub`, `match`
exhaustiveness, interfaces, generics with PEP 695, pipes, guards).
Each is structured the way you would actually ship it: a `typhon.toml`,
a `src/` tree split by responsibility, and a README documenting how to
run it and which language features it leans on.

The first ten were built in two rounds against tyc 0.5.2 and shaped
the v0.6.0 fixes. The next five (11–15) were built in a third round
against **tyc 0.6.1** to push areas the first two rounds didn't
exercise — async middleware, multi-stage heterogeneous-error pipelines,
generic stream operators, an HNSW vector index, and an async
multiplayer game server. The consolidated findings live in
[`TYPHON_FEEDBACK.md`](TYPHON_FEEDBACK.md).

| # | Project | What it is | Key features exercised |
|---|---|---|---|
| 1 | [`01-task-scheduler`](01-task-scheduler/) | Distributed task scheduler with worker pool, retry/backoff, DAG dependencies, HTTP control plane | `gather:`, `go`, sealed-union events, `newtype` ids, `freeze let` config, `Result` chains, `impl` for state machines |
| 2 | [`02-trading-engine`](02-trading-engine/) | Limit-order book + matching engine + risk + market-data fan-out | `newtype` for money/price/qty, `frozen class`, `pub`, exhaustive `match` over order events, `comptime let` for venue constants |
| 3 | [`03-ml-orchestrator`](03-ml-orchestrator/) | ML pipeline orchestrator: datasets, training jobs, sweeps, model registry | `lazy import` numpy/torch, generics with PEP 695 over pipeline stages, `with`-chain `Result` plumbing, sealed-union job states |
| 4 | [`04-event-sourced-banking`](04-event-sourced-banking/) | Event-sourced double-entry ledger with projections, snapshots, FX, AML | sealed-union events and commands, `newtype` for money/account IDs, pattern matching projections, `freeze let` rate cards |
| 5 | [`05-web-crawler`](05-web-crawler/) | Concurrent crawler + content extractor + summariser with politeness/robots | `gather(strategy="best-effort")`, `go`, `lazy import`, retry/backoff, `Result[T, E]` everywhere, rate limits |
| 6 | [`06-graphql-server`](06-graphql-server/) | Typed GraphQL-style query engine: schema, recursive resolvers, batched `DataLoader[K, V]`, role-based auth | generic `class[K, V]` + `impl[K, V]`, recursive query AST, `Result` chains, `Callable` fields, `?` propagation in `pub def` |
| 7 | [`07-game-ecs`](07-game-ecs/) | In-process entity-component-system game engine with system scheduler and event queue | frozen value components, sealed-union `Component` / `GameEvent`, `Callable`-typed system functions, dense exhaustive `match` |
| 8 | [`08-mini-compiler`](08-mini-compiler/) | Lexer + Pratt parser + type checker + tree-walking interpreter for a tiny expression language | 13-variant recursive `Expr`, cross-module recursive `Env` ↔ `VFn`, four-stage `Result` pipeline with heterogeneous error types |
| 9 | [`09-search-engine`](09-search-engine/) | Full-text search engine: tokenizer, inverted index, BM25 ranking, AND/OR/NOT/phrase query parser, snippet highlighting | recursive `Query` AST, `freeze let` constants, frozen `Posting` with `tuple[int, ...]`, BM25 math over `newtype` IDs |
| 10 | [`10-distributed-kv`](10-distributed-kv/) | Simulated 3-node Raft-lite KV store: leader election, log replication, majority commit, client put/get | 7-variant `Message` union, role state machine, deterministic simulated message bus, log-index / term arithmetic |
| 11 | [`11-realtime-game-server`](11-realtime-game-server/) | Async multiplayer game server with lobby + matchmaking by ELO, per-room tick loop, pluggable game modes, FastAPI control plane, SQLite player profiles | `gather:`, `go`, generic `RingBuffer[T]`, sealed-union events + room states, ELO math via `newtype Rating`, `Callable`-field mode strategies |
| 12 | [`12-static-site-gen`](12-static-site-gen/) | Static site generator: Markdown lexer/parser, template engine with `{% extends %}` inheritance, RSS/sitemap, SQLite incremental-build manifest | two recursive sealed-union ASTs (`Block` / `Inline`, `TplNode`), four-stage heterogeneous-error pipeline with envelope error union, `freeze let` escape table |
| 13 | [`13-vector-db`](13-vector-db/) | In-memory vector database: HNSW index, pluggable distance metrics, recursive filter DSL (lex + parse + eval), snapshot persistence, FastAPI server | generic `Collection[D]`, sealed-union `Metric` + `FilterExpr` with factories, `lazy import np = numpy`, `async with` RW-lock |
| 14 | [`14-api-gateway`](14-api-gateway/) | L7 API gateway / service mesh: route matching, load-balancing strategies, circuit breakers, retry policy, token-bucket rate limiting, health probes, `/metrics` | `Callable`-field middleware pipeline (workaround for cross-module interface limit + R3-1 async-await-on-Callable bug), `BreakerState` state machine, `freeze let` policy constants |
| 15 | [`15-stream-processor`](15-stream-processor/) | Flink-lite stream processor: source/map/filter/keyBy/window/aggregate/sink operators, event-time watermarks, keyed state with snapshot/restore | generic `ListSource[T]` / `Operator[I, O]`, sealed-union `OperatorKind` / `WindowKind`, `gather:` + `go` topology, watermark arithmetic via `newtype WatermarkTs` |

Each app has its own README with the run recipe. None of them require a
network or third-party services to *type-check* — `tyc check` runs end
to end on every project regardless of whether the runtime deps are
installed.

## Running an app

```bash
cd examples/apps/01-task-scheduler
tyc check src/        # parse + resolve + type-check
tyc build             # emit clean Python to build/
python build/main.py  # run it (after `tyc sync` for runtime deps)
```

## Reading the source

Each app's `src/` is split top-down:

- `models.ty` — `model` types at trust boundaries (HTTP/JSON) + internal `class` types
- `domain.ty` / `engine.ty` / `core.ty` — the heart of the app
- `store.ty` / `repo.ty` — persistence
- `api.ty` / `server.ty` — HTTP / control plane (where applicable)
- `main.ty` — entry point

The point of these examples is to *show* a working architecture in
Typhon — not just one syntactic feature in isolation. Use them as a
template when starting a non-trivial project.
