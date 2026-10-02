# 16 — Shape catalogue (multi-file layout probe)

A small, deliberately *structural* app: its job is to exercise the
cross-module shapes that single-file examples cannot — the ones the
2026-09-30 release-readiness review found gaps in.

- **Relative imports inside a package** — `shapes/ops.ty` reaches its
  sibling with `from .kinds import Shape, …`, and `catalogue/store.ty` uses
  `from .text import describe`. The sealed union, the `newtype` and the
  `frozen` classes must all survive that route (they did not, before the
  review: a relative import was looked up as an absolute one).
- **`pub *` facades** re-export everything, including tables a facade used
  to drop — the `newtype ShapeId` (which must still widen to `int`, and
  must still be *required* where a `ShapeId` is expected), the `frozen`
  classes (a write to `Circle.r` through the facade must be rejected) and
  the `enum Kind`.
- **Sealed-union exhaustiveness across modules** — every `match` over
  `Shape` in `ops.ty`, `text.ty` and `main.ty` is checked for all three
  variants.
- **`extend str` on call receivers** — `describe(s).slug()` and a literal
  f-string receiver are lowered to the lifted free function on both the
  compiled path and the VM. Note the import: `main.ty` takes `describe`
  from `catalogue.text` directly, because an extension travels with the
  module that declares it, not through a `pub *` facade.
- **`with`-chains over a cross-module `Result`** — `report` unwraps
  `Catalogue.require` with `with s = cat.require(sid)?:` and formats the
  `else err:` binding.

## Run

```bash
tyc check src
tyc build --no-sync && python3 build/main.py
tyc run              # the VM must print exactly the same lines
```

Output is deterministic, so the VM ↔ CPython differential gate compares it
byte for byte.
