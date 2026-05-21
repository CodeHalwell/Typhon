# Stress Round 2026-05-21

Fresh round of `.ty` stress probes spanning 10 categories. The
write-up (findings B1–B14) is consolidated under
[`docs/findings.md`](../../docs/findings.md) — see the "Open
findings" section (O1–O29) for the full catalogue and the "Sprint
history → May 21 round" section for the per-finding mapping.

## Run

```bash
./run_one.sh 01-language-edge/01-let-mut-shadowing.ty   # one file
./run_all.sh                                             # whole suite
SHOW_EMIT=1 ./run_one.sh 02-io/02-json-roundtrip.ty      # dump emitted .py too
```

A sidecar `<name>.deps` file adds entries to `[dependencies]` for that
case (one package per line). Used by `03-ml-numpy/02-numpy-ops.deps`
etc.

## Summary

- 81 cases authored, 65 build + run clean.
- 8 of the 16 failures are deliberate diagnostic probes in
  `10-error-quality/`.
- 7 real compiler bugs surfaced, all tracked as open findings in
  `docs/findings.md` (B1–B14 maps to O1–O29 as listed in that doc).
