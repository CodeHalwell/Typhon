# Stress-test corpus — 2026-05-19 campaign

Repro artifacts for the FINDINGS.md "2026-05-19 stress-test campaign"
section (findings #57–#96).

- `tests/` — 127 hand-written `.ty` programs that exercise documented
  language surface and edge cases.
- `run.sh` — run `tyc check` against each test, dump output to `out/`.
- `build_run.sh` / `build_run13.sh` — build each test as a project and
  attempt to run the emitted Python (the `13` variant uses python3.13).
- `migrate_src.py` / `migrate_src.ty` — input + (broken) output for the
  `tyc migrate` finding (#64).
- `fmt_test.ty` / `fmt2.ty` — inputs preserved for the `tyc fmt`
  no-op finding (#65).
- `trace_test.ty` — small program for `tyc trace` smoke test (works).

To re-run from scratch:
```bash
cd stress
bash run.sh            # writes out/ summaries
bash build_run13.sh tests/*.ty   # builds + runs each, requires python3.13
```
