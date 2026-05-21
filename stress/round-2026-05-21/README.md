# Stress Round 2026-05-21

Fresh round of `.ty` stress probes spanning 10 categories. The
deliverable is **FINDINGS.md** in this directory — see it for the bug
catalogue and recommendations.

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

- 76 cases authored, 65 build + run clean.
- 8 of the 11 failures are deliberate diagnostic probes in
  `10-error-quality/`.
- 8 real findings (B1–B14, see FINDINGS.md), ranging from a critical
  narrowing bug (B5) to a UX nit on the REPL (B13).
