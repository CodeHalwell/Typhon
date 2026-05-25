# 15 — Stream processing engine (Flink-lite)

A production-shaped real-time stream processing engine with:

- **Source operators** — `ListSource[T]` (fixture-driven) and `GeneratorSource[T]` (synthesises a deterministic walk of events) for emitting `Event[T]` records
- **Stream operators** — `map`, `filter`, `keyBy`, `window` (tumbling + sliding by event-time), `aggregate` (sum/count/min/max/avg), `reduce`, windowed `join`
- **Generic `Stream[T]`** with chainable operator builders; operators are typed as `Operator[I, O]`
- **Watermarks** — bounded-out-of-orderness generator; window emission fires when the watermark passes the window's end-time, and late events are routed to a side-output sink
- **Sink operators** — `PrintSink`, `BufferSink` (collects into a list for assertions), `CallbackSink` (calls a user `Callable[[T], None]`)
- **Topology DAG** — sources → operators → sinks, built fluently
- **Async execution** — operators run as coroutines exchanging envelopes via `asyncio.Queue`s; the runner spawns them with `go` and `gather:`, propagates watermarks, and handles graceful shutdown via EndOfStream envelopes
- **Keyed state backend** — per-operator `dict[key, AggregateCell]` with `snapshot()` / `restore()` for at-least-once recovery semantics
- **At-least-once processing** — every consumed envelope is journaled with an offset; sinks `ack` once accepted, and `replay_from(offset)` reads the uncommitted suffix
- **CLI** that runs a built-in "stock-price moving average" demo job end-to-end

## Files

| File | Responsibility |
|---|---|
| `src/ids.ty` | `newtype` wrappers (`OperatorId`, `JobId`, `WatermarkTs`, `Offset`, `WindowId`, `StreamKey`) |
| `src/models.ty` | Shared classes (`StockTick`, `AggregateRow`, `JoinedRow`) and the `StreamError` sealed union with factory helpers |
| `src/config.ty` | `freeze let` + `comptime let` constants; `validate_config()` returning `Result[JobConfig, StreamError]` |
| `src/event.ty` | `Event[T]` generic record + `Envelope` wire frame + `EnvelopeKind` sealed union (Record/Watermark/Barrier/EndOfStream) with factory functions |
| `src/watermark.ty` | Bounded-out-of-orderness `WatermarkGenerator`; `is_late(...)` predicate |
| `src/source.ty` | `Source` concrete carrier with `ListSource[T]` and `GeneratorSource[T]` builders |
| `src/keyed_state.ty` | `KeyedAggState` per-operator dict with snapshot/restore |
| `src/window.ty` | `WindowKind` sealed union (Tumbling/Sliding/Session), `Pane` + `pane_to_row` |
| `src/stream_op.ty` | `OperatorKind` sealed union, `Stream[T]`, `Operator[I, O]`, and `make_map_op`/`make_filter_op`/`make_key_by_op`/`make_reduce_op`/`make_join_op` builders |
| `src/sink.ty` | `Sink` carrier with `PrintSink` / `BufferSink` / `CallbackSink` builders + `LateSideOutput` for late records |
| `src/topology.ty` | `Topology` DAG (sources/operators/sinks/nodes), with `add_source`, `add_operator`, `add_sink`, `add_join` |
| `src/runner.ty` | Async job `Runner` — coroutines per operator over `asyncio.Queue`s, watermark-driven window flush, in-memory `Journal` |
| `src/demo_job.ty` | Builds + runs the stock-price moving-average demo using `gather:` and `go ... -> task` |
| `src/cli.ty` | Sub-command dispatcher (`demo` / `collect` / `describe` / `help`); exercises `Result`-based config loading with `match` |
| `src/main.ty` | Entry point — calls `dispatch(sys.argv)` |

## Features exercised

- `newtype` for every ID kind: `OperatorId`, `JobId`, `WatermarkTs` (newtype int representing ms epoch), `Offset`, `WindowId`, `StreamKey`
- `freeze let` for window/lateness/queue-capacity constants
- `comptime let` for build tag and default sizes
- Generic classes: `Event[T]`, `Stream[T]`, `Operator[I, O]`, `ListSource[T]`, `GeneratorSource[T]`
- Sealed unions: `EnvelopeKind`, `OperatorKind`, `WindowKind`, `StreamError`
- Exhaustive `match` over each sealed union (with `raise RuntimeError("unreachable")` trailers per TYPHON\_FEEDBACK round-1 #5)
- `Result[T, E]` with three distinct error variants (`ConfigError`, `OperatorError`, `SinkError`) and `?` propagation in `loaded_config`
- `gather:` for parallel source/window/sink coroutines
- `go runner.run_source(...) -> src_task` for spawning with strong-ref registry
- `asyncio.Queue` between operators
- Factory functions defined in each sealed union's own module (round-1 #1 workaround)
- Concrete-class-with-Callable for `Source` and `Sink` carriers (round-1 #2 workaround)

## Running

Build `tyc` first if it isn't on PATH:

```bash
export PATH=/home/user/Typhon/tyc/target/release:$PATH
```

Then:

```bash
cd examples/apps/15-stream-processor
tyc check src/
tyc build
python3.13 build/main.py demo       # runs the moving-average demo
python3.13 build/main.py collect    # runs a buffer-sink variant
python3.13 build/main.py describe   # prints the topology DAG
```

Python 3.13+ is required at runtime (PEP 695 `type` aliases). The build/runtime
chain is otherwise dependency-free — stdlib only.
