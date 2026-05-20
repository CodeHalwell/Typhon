# Stress-test corpus — 2026-05-19 fresh round

Repro artifacts for the FINDINGS.md "2026-05-19 fresh round" section
(findings #97–#127).

- `cases/` — 107 hand-written `.ty` programs targeting underexplored
  surface: `typing` bridge types (Self/Literal/Final/ClassVar/
  Annotated/TypedDict/NamedTuple/Protocol/TypeGuard), iterator/
  descriptor/context-manager protocols, multiple inheritance,
  `__call__`/`__post_init__`/`@property.setter`, debug `{x=}`
  f-strings, recursive type aliases.
- `torture/` — 8 deliberately-malformed inputs to verify the parser
  emits diagnostics without panic.
- `multifile/` — a 2-module project that builds and runs end-to-end.
- `playground/` — a fresh `tyc init`-scaffolded project, used to
  inspect the default `typhon.toml` and entry point.
- `run_one.sh` — `tyc check FILE`, dump full output.
- `build_one.sh` — copy FILE into a transient project, `tyc build`,
  print the emitted Python, then run it under `python3.13`.

The helper scripts locate `tyc` via `git rev-parse --show-toplevel` by
default, so they work from any clone without `TYC=...` once the release
build exists. To re-run from scratch, from the repo root:

```bash
cargo build --release --manifest-path tyc/Cargo.toml   # one-time
cd stress/round-2026-05-19
for f in cases/*.ty; do ./run_one.sh "$f"; done
for f in cases/*.ty; do ./build_one.sh "$f"; done
```

Override with environment variables:

- `TYC=/path/to/tyc` — point at a different compiler binary.
- `PYTHON=python3.14` — pick the interpreter for `build_one.sh`.
