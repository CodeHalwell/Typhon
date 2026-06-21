# Performance Baseline

This document records the first committed benchmark baseline for Typhon's
compilation pipeline. All measurements were taken on the same commit that
introduced this file; re-run locally to establish a new baseline after
significant pipeline changes.

## How to run

```bash
cd tyc
cargo bench -p tyc-syntax --bench parse_preprocess
cargo bench -p tyc-db    --bench incremental_check
```

Criterion writes an HTML report to `tyc/target/criterion/`. Open
`tyc/target/criterion/report/index.html` in a browser for the full
per-benchmark breakdown and history graphs.

## Threshold policy

> **20 % regression on any benchmark is a review blocker.**

If Criterion reports `Performance has regressed` with a change above +20 %,
the PR that introduced the regression must either justify it (larger modules
tested, new pass added) or revert it before merging. The threshold is set
conservatively — CI machines vary; changes under 20 % are noise on
different hardware.

## Baseline (first recorded)

Recorded on a cloud CI runner (Linux x86-64). These numbers represent the
**lower bound** on real developer machines; expect 1–3× faster on
contemporary laptop or desktop hardware.

### `tyc-syntax` — preprocess + parse latency

The preprocessor strips Typhon line-prefix keywords (`let`/`mut`/`model`/
`interface`/`unsafe`/etc.) and expands sugar (`T?`, `|>`, `with`-chains,
`gather:`, `go`, `lazy import`). The parser is `ruff_python_parser` operating
on the preprocessed Python source.

Fixture sizes:
- **small** — ~15 lines, basic bindings, class, two functions
- **medium** — ~100 lines, classes, interface, several functions
- **large** — ~250 lines, many classes and functions, deeply nullable signatures

| benchmark                        | median  | 99th pct |
|----------------------------------|---------|----------|
| `preprocess/small/module`        |  2.5 µs |  2.6 µs  |
| `preprocess/medium/module`       | 11.5 µs | 11.7 µs  |
| `preprocess/large/module`        | 26.5 µs | 26.9 µs  |
| `parse/small/module`             | 44.4 µs | 45.0 µs  |
| `parse/medium/module`            | 223 µs  | 227 µs   |
| `parse/large/module`             | 528 µs  | 537 µs   |
| `preprocess_then_parse/small`    | 52 µs   | 54 µs    |
| `preprocess_then_parse/medium`   | 242 µs  | 247 µs   |
| `preprocess_then_parse/large`    | 594 µs  | 605 µs   |

**Verdict:** all fixtures complete well under 1 ms. The project's stated
sub-100 ms incremental feedback target has ample headroom at these module
sizes.

### `tyc-db` — end-to-end check latency

`check_file` runs preprocess → parse → resolve → type-check and returns
diagnostics. The `second_check` benchmark measures the cost of a re-check
after a small edit (one integer literal changed); because resolve and
type-check are not yet Salsa tracked queries, this currently re-runs the
full pipeline rather than a true incremental delta.

| benchmark                          | median   | 99th pct |
|------------------------------------|----------|----------|
| `cold_check/small/module`          |  81 µs   |  83 µs   |
| `cold_check/medium/module`         | 232 µs   | 238 µs   |
| `cold_check/large/module`          | 1.58 ms  | 1.60 ms  |
| `second_check/small content change`|  85 µs   |  88 µs   |

**Verdict:** cold checks on realistic modules complete well under 10 ms.
Once resolve and type-check are migrated to Salsa tracked queries, the
`second_check` latency should drop to near-zero for unchanged nodes.

## Re-baselining procedure

1. Checkout a clean main branch.
2. Run both bench suites as shown above.
3. Copy the `time: [lo median hi]` lines from the Criterion output into
   the table above, replacing the existing values.
4. Commit the updated table with the message
   `chore: update performance baseline (YYYY-MM-DD)`.
5. Do not baseline on a machine under unusually high load; Criterion
   measures wall time and background processes inflate results.

---

## CI-enforced build-pipeline regression gate (alpha-plan F2)

The Criterion suites above measure individual passes in microseconds and serve
as a *review aid*. Separately, CI runs an automated gate over the **whole
`tyc build` pipeline** — preprocess → parse → check → comptime → desugar →
emit → format — and **fails the build** when it regresses beyond a threshold.

### What it measures

`scripts/perf-gate.sh` times the release binary running:

```bash
tyc build examples/47-mini-app --no-sync --check
```

- **Fixed corpus:** `examples/47-mini-app` — a real, self-contained ~356-line
  Typhon project (multiple modules, classes, async/gather, `Result`, nullable
  signatures) that exercises every pipeline stage.
- **Network-free & non-destructive:** `--no-sync` skips `uv sync` (no network);
  `--check` runs the full pipeline as a dry run without writing output.
- **Median of N runs** (default 9, after 2 untimed warmup runs) to absorb
  runner noise. The median ignores the occasional slow outlier that a mean
  would let through.

The script depends only on `bash`, `python3`, and `jq` (all present on
`ubuntu-latest`) and adds **no Rust build targets or crate dependencies**, so it
stays disjoint from compiler work.

### Pass / fail decision

The committed baseline lives in `perf-baseline.json` at the repo root:

```json
{ "median_ms": 89, "corpus": "examples/47-mini-app", "threshold": 0.20, ... }
```

The gate computes `limit = median_ms * (1 + threshold)` and **fails (exit 1)**
when the measured median exceeds that limit. With the default 20 % threshold and
an 89 ms baseline, builds slower than ~107 ms fail. Anything at or under the
limit passes.

### Threshold rationale and false-failure tradeoff

CI runners are noisy, so a tight gate would flake. The design minimises false
failures three ways: (1) take a **median**, not a single sample or a mean;
(2) use a **generous 20 % threshold** — small, legitimate drifts pass; (3) run
**warmup iterations** first so cold caches don't count. In practice the local
median spread is ~1–3 ms (88–90 ms) and even a noisy run staying under +20 %
passes, so the gate only trips on a **large, sustained regression** — which is
exactly what F2 asks for. The cost of this choice is that a *small* genuine
regression (say +10 %) slips through; that is an intentional trade for a
trustworthy, non-flaky hard gate.

### Running it locally

```bash
# build the release binary first
(cd tyc && cargo build --release --bin tyc)

# gate against the committed baseline (exit 1 on regression)
scripts/perf-gate.sh

# refresh the baseline after an intentional, justified change
scripts/perf-gate.sh --update     # measures, then rewrites perf-baseline.json
```

Useful env overrides: `PERF_RUNS`, `PERF_WARMUP`, `PERF_THRESHOLD` (fraction,
e.g. `0.20`), `PERF_BASELINE` (path), `TYC_BIN` (binary path).

### Refreshing the baseline

Only refresh for an **intentional** change that legitimately moves the number
(a new pass, a larger corpus, a deliberate trade-off). On a quiet machine:

1. `(cd tyc && cargo build --release --bin tyc)`
2. `scripts/perf-gate.sh --update`
3. Commit `perf-baseline.json` with a message noting *why* the baseline moved.

The CI runner is slower than a typical laptop, so the committed number is a
conservative upper bound; expect local runs to be faster.
