# 03 — ML pipeline orchestrator

A pipeline orchestrator for ML jobs. The pipeline itself is generic:
each `Stage[I, O]` consumes one type and produces another, and stages
compose into typed `Pipeline[I, O]` instances that the runner executes.
On top of that sits a **registry**, an **experiment tracker**, and a
**model store** — the production-shaped scaffolding that surrounds
the algorithms.

- **Generic typed stages** — `Stage[I, O]` is a generic class whose
  `fn` field is a `Callable[[Batch[I]], Result[Batch[O], PipelineError]]`,
  built by per-stage factory functions (e.g. `standardise() -> Stage[...]`).
  An earlier draft used an `interface Stage[I, O]`, but cross-module
  interface conformance isn't accepted by tyc 0.5.2 (see
  `examples/apps/TYPHON_FEEDBACK.md` §2); the class-of-Callable form
  preserves the same typed-pipeline shape and composes the same way.
- **Sealed-union job states** — `Pending | Running | Succeeded | Failed`
- **Hyperparameter sweep** — grid + random search runners over a job spec
- **Dataset registry** — versioned datasets, content-hashed
- **Model registry** — name + version + metrics
- **Experiment tracker** — per-run metrics persisted to JSONL
- **Pluggable backend** — numpy by default, swap in torch via `lazy import`

## Files

| File | Responsibility |
|---|---|
| `src/domain/ids.ty` | `newtype` IDs for datasets, runs, jobs, models |
| `src/domain/datatypes.ty` | Generic typed samples and batch wrappers |
| `src/pipeline/stages.ty` | `interface Stage[I, O]` + several `impl` stages (scale, split, train, evaluate) |
| `src/pipeline/pipeline.ty` | `Pipeline[I, O]` composition with `Result`-returning execution |
| `src/registry/registry.ty` | Dataset + model + run registries |
| `src/registry/tracker.ty` | JSONL experiment tracker |
| `src/pipeline/sweep.ty` | Grid and random sweep runners |
| `src/main.ty` | Worked end-to-end example training a tiny linear model |

## Features exercised

- Generic types `Stage[I, O]`, `Pipeline[I, O]`, `Sample[T]`, `Batch[T]` (PEP 695)
- `lazy import np = numpy` — numpy isn't touched at import time
- `newtype` for every ID kind
- Sealed-union job states with exhaustive `match`
- `Result[T, PipelineError]` returned by every stage; `with`-chained `?` in `run`
- `freeze let` global config
- `comptime let` for build-time constants
- `pub` markers on every public symbol
- `interface` for `Stage` and `MetricsSink`

## Running

```bash
cd examples/apps/03-ml-orchestrator
tyc check src/
tyc build
python build/main.py
```

The default `main.py` builds a synthetic dataset, runs a 3-stage
pipeline through a tiny grid sweep, and writes results to
`/tmp/typhon-experiments.jsonl`.
