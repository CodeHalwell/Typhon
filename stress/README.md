# Stress-test corpus (internal)

> **Internal reproduction corpus.** This directory holds the raw artifacts
> from Typhon's adversarial stress-test campaigns — deliberately including
> programs that are *expected to fail* (negative fixtures, torture cases,
> minimised bug reproducers). It is not a tutorial or an example set; for
> curated, compile-clean programs see [`../examples/`](../examples/). The
> campaigns run from May 2026 onward and live in the dated `round-*/`
> directories plus `pypi-sweep/` and `third-party-py-corpus/`; the most
> recent round doubles as release-verification input. Consolidated findings
> are in [`docs/findings.md`](../docs/findings.md).

## 2026-05-19 campaign

Repro artifacts for the May 19 stress-test campaign (originally findings
#57–#96, now consolidated under [`docs/findings.md`](../docs/findings.md)
in the "Sprint history" section).

- `tests/` — 127 hand-written `.ty` programs that exercise documented
  language surface and edge cases. Top numeric prefix is 120; the
  extra seven are lettered variants (`05b`, `09b`, `40b`, `67b`,
  `67c`, `69b`, `87b`) that minimise reproducers for specific bugs.
  Verify with `ls tests/*.ty | wc -l`.
- `run.sh` — run `tyc check` against each test, dump output to `out/`.
- `build_run.sh` — build each test as a project and run the emitted
  Python.
- `migrate_src.py` / `migrate_src.ty` — input + (broken) output for the
  `tyc migrate` finding (#64).
- `fmt_test.ty` / `fmt2.ty` — inputs preserved for the `tyc fmt`
  no-op finding (#65).
- `trace_test.ty` — small program for `tyc trace` smoke test (works).

Both scripts auto-locate `tyc` at `<repo-root>/tyc/target/release/tyc`
via `git rev-parse --show-toplevel`. Override with environment
variables:

- `TYC=/path/to/tyc` — point at a different compiler binary.
- `PYTHON=python3.14` — pick the interpreter for `build_run.sh`
  (default `python3.13`; Typhon targets 3.13+ so older Pythons will
  reject things like nested f-strings and PEP 695 generics).

To re-run from scratch:
```bash
cargo build --release --manifest-path ../tyc/Cargo.toml   # one-time
cd stress
bash run.sh                       # writes out/ summaries
bash build_run.sh tests/*.ty      # builds + runs each
```
