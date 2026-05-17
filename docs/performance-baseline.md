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

The preprocessor strips Typhon line-prefix keywords (`val`/`var`/`model`/
`interface`/`unsafe`/etc.) and expands sugar (`T?`, `|>`, `with`-chains,
`gather:`, `go`, `lazy import`). The parser is `rustpython-parser` operating
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
