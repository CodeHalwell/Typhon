# `corpus/valid/` — the valid-programs corpus (W6-18)

Real, ordinary Python libraries, run through `tyc migrate`, fixed up once by
hand, and kept green. The `examples/` + `stress/` corpora are mostly programs
written to break the checker; the W1-01 and W1-02 false-positive regressions
got through because nothing checked that ordinary code still passes. Every new
diagnostic is now swept over this directory automatically by the `valid-corpus`
CI job, which fails if any unit here stops checking clean or changes behaviour
(VM vs CPython differential, `scripts/vm-differential.sh --scope valid`).

## Provenance

Seeded from CPython 3.13's standard library (PSF-2.0, same licence family as
the corpus sources). Each file keeps its upstream docstrings and comments;
only the minimum edits needed for `tyc check` were made, and every semantic
deviation from upstream is listed below. Line counts are for the `.ty` files
including the 4-line provenance header.

| unit | upstream | lines | hand-fix summary |
| --- | --- | --- | --- |
| `bisect.ty` | `Lib/bisect.py` | 151 | `[T]` generics, `hi: int?`, `key: Callable[[T], T]?`, `mut` loop cursors, `hi_resolved` rename |
| `colorsys.ty` | `Lib/colorsys.py` | 171 | float annotations, pre-declared `mut` tuple unpacks, `hue_wrapped`, `margin: str?` + `guard`, `_is_nonspace` helper (a `lambda` with a default is not expressible) |
| `copy.ty` | `Lib/copy.py` | 310 | `dict[type, object]` dispatch tables (temp `d` inlined), `object()` unique-sentinel default for `_nil`, `memo_table` None-narrowing, `seq`/`args_gen`/`copied`/`key2` renames, `as!` only where noted below |
| `fnmatch.ty` | `Lib/fnmatch.py` | 194 | `str \| bytes` entry preserved with an `object` return for the bound `.match`, `empty_collection` annotations, `fixed_str`/`joined` renames, `as! str` after `is not STAR` guards (the guard cannot narrow `object`) |
| `heapq.ty` | `Lib/heapq.py` | 635 | `[T]` generics over `list[T]` (bare-`T` `<` comparisons check), `cur` loop temporaries, `heappushpop` swap via early `return smallest`, `merge` as `-> Iterator[object]` with hoisted `object`-typed dispatch slots, `nsmallest`/`nlargest` with hoisted `mut it` and per-path `pairs`/`triples`/`top`/`top2` names |
| `string.ty` | `Lib/string.py` | 329 | valueless `pattern: _re.Pattern[str]` class declaration (non-default first), `__init_subclass__` early-return restructure (a custom *string* `pattern` still compiles with `cls.flags`, as upstream), `auto_arg_index: int \| bool` via `auto_index`, fixed-arity `tuple[str, int \| bool]` plumbing, `field_str`/`expanded`/`auto_next` renames |
| `textwrap.ty` | `Lib/textwrap.py` | 502 | annotated class-level patterns (`re.Pattern[str]`, `dict[int, str]`), `_split_chunks -> list[str]` (a migrate transcription slip, not upstream), `dedent` via `guard` + `cleaned`, `_is_nonspace` helper |

Total: ~2,300 lines. The corpus is a seed, not a ceiling — add more migrated
libraries over time (see below).

## Known semantic deltas from upstream

1. `copy.ty`: `slice` is absent from `_copy_dispatch` because the `slice`
   builtin is not in the checker's scope (`tyc::unknown_name` — a checker
   gap). Verified on CPython 3.13 that the table-miss path reconstructs an
   equal slice via `__reduce_ex__`, so `copy`/`deepcopy` still return
   `slice(1, 2, 3)` for `slice(1, 2, 3)`; only object *identity*
   (`copy.copy(s) is s`) differs for slices.
2. `copy.ty`: `_nil` defaults to `object()` instead of `[]`. Both are
   single unique sentinel instances, so the `is not _nil` memo protocol is
   unchanged; only `deepcopy.__defaults__` repr differs.
3. `string.ty`: `braceidpattern: str?` and `flags: object` are wider than the
   upstream values (`None`, `re.IGNORECASE`); every runtime value still checks.
4. `heapq.ty`: `nsmallest`/`nlargest` build `pairs`/`triples` with an explicit
   loop instead of a comprehension, and `merge` binds `pair`/`pair2` instead
   of unpacking `enumerate` — same lists, checker-legible bindings.
5. `fnmatch.ty` / `string.ty` / `heapq.ty`: `filter` takes `list[str]`,
   `Formatter.format` takes `*args: object` — annotation-time narrowings of
   "any iterable" parameters, applied where the checker needs a concrete
   element type.

## Differential status (seed triage, 2026-10-03, tyc 1.0.0-alpha.9)

`bisect`, `colorsys`, `copy`, and `fnmatch` agree byte-for-byte on both
surfaces (empty output, exit 0); `heapq` agrees including its `__main__`
doctests (`failed=0, attempted=2` on both sides). `textwrap` runs fine on the
CPython side (including its `__main__` demo) but the VM crashes at import
(`dict.fromkeys` is not modelled) — pinned in
`scripts/valid-corpus-baseline.txt` as a VM bug to burn down.

`string.ty` is `vacuous-runtime`: both surfaces crash identically because the
emitter mishandles the explicit `Template.__init_subclass__()` call upstream
relies on (an `impl` method with a `cls` first parameter emits as
`(self, cls)` with no `@classmethod`). The unit stays — it is faithful to
upstream and the check-clean sweep is the corpus's primary job — and it flips
to real coverage on its own once the emitter is fixed. The VM additionally
falls back to `--compile` for `string.ty` (`_string` is unmodelled), so the VM
never executes it today.

## Advice warnings are tolerated

The gate is errors-only. `string.ty` currently carries one warn-level
`tyc::lazy_import_opportunity` (`import _string` at module scope, used only in
method bodies) — kept as-is to stay close to upstream. Do not "fix" warnings
by restructuring units unless the restructure is semantics-neutral.

## Adding a unit

1. Copy the upstream file next to the corpus and run `tyc migrate <file>.py`.
2. Hand-fix to `tyc check` clean, keeping runtime semantics identical; prefer
   renames and `as!` casts over restructures, and record any unavoidable delta
   in the table above.
3. Add the provenance header (see any unit), extend the table, and run both
   gate commands below before committing.

## Gate commands

```bash
tyc check corpus/valid/
scripts/vm-differential.sh --scope valid --baseline scripts/valid-corpus-baseline.txt
```

New divergences go through the same triage as the main differential baseline:
a divergence is a VM/emitter bug, a unit bug, or an environment fact
(`vacuous`) — never silently baselined.
