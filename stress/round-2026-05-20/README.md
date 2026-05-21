# Stress-test corpus — 2026-05-20 fresh round

Repro artifacts for the May 20 round (originally findings R3.1–R3.18,
now consolidated under [`docs/findings.md`](../../docs/findings.md)).

- `cases/` — 140 hand-written `.ty` programs targeting:
  - Result / `?` propagation depth and edge positions (cases 01–05, 60, 97, 111).
  - Pipe lowering (cases 06–08).
  - Walrus + paren stripping (cases 46–47, 56).
  - Match exhaustiveness shapes (cases 11–15, 26–27, 61, 101–102, 108, 124, 140).
  - `gather:` strict + best-effort + `go` (cases 16–19).
  - Comptime literals / arithmetic / def (cases 20–22).
  - Purity rules (cases 23–25, 112–115).
  - Frozen / dataclass / Pydantic / class constants (cases 40–42,
    121–122, 126, 129).
  - Lazy import / lazy let (cases 28–31, 127).
  - Unsafe boundary (cases 32–33).
  - Generics + interfaces (cases 36–39).
  - Sealed unions + pattern dispatch (cases 65, 68–69).
  - Operator type-check / index bounds (cases 130–131, 134).
  - Multi-file (`/tmp/mf_test`), tyc fmt (`/tmp/tyfmt`), tyc trace
    (`/tmp/trace_test`), tyc init (`/tmp/init_test`), .dty stubs
    (`/tmp/stub_test`) — out-of-tree.
- `build_one.sh` / `run_one.sh` — same helpers as the 2026-05-19 round.

The helper scripts locate `tyc` via `git rev-parse --show-toplevel` by
default, so they work from any clone without `TYC=...` once the release
build exists. To re-run from scratch, from the repo root:

```bash
cargo build --release --manifest-path tyc/Cargo.toml
cd stress/round-2026-05-20
for f in cases/*.ty; do ./build_one.sh "$f"; done
```

Override with environment variables:

- `TYC=/path/to/tyc` — point at a different compiler binary.
- `PYTHON=python3.14` — pick the interpreter for `build_one.sh`.
