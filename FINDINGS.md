# Typhon — Stress-Test Findings

Date: 2026-05-18
Branch: `claude/test-typhon-library-EnZmE`
Compiler: `tyc` built from this commit, `cargo build --release`.

This document records what happened when I deliberately tried to break Typhon
by writing dozens of `.ty` programs that exercise the documented surface and
the edges around it. Each finding has:

- **Severity** — `bug` (incorrect behaviour), `gap` (documented feature
  unimplemented / partly implemented), `papercut` (works but UX is poor),
  `doc` (docs vs reality drift), `enhancement` (suggested addition).
- **Repro** — minimal `.ty` source plus the exact `tyc` invocation.
- **Observed** — the actual output (often a panic or unhelpful diagnostic).
- **Expected** — what the docs/skill imply should happen.

Findings are grouped by area. Numbering is global so issues can be referenced.

---

## Executive summary

I built `tyc` from this commit (release profile, clean ~70s build) and ran
~45 hand-written `.ty` programs through `tyc check` / `tyc build` /
`tyc fmt` / `tyc migrate` / `tyc trace` / `tyc repl` / `tyc add`. I tried
to break every documented feature and a handful of undocumented ones.

**What works well:**

- The Salsa-backed pipeline is fast and the diagnostics are uniformly
  miette-pretty when they fire.
- Result / `?` propagation / `with`-chain lowering is solid for matching
  error types — desugars cleanly and produces tight Python.
- Exhaustive `match` on sealed unions works.
- Inheritance + nominal-subtyping assignments work.
- `extend BUILTIN:` end-to-end (write, build, run) works.
- `tyc trace` correctly maps Python tracebacks back to `.ty` line numbers.
- `tyc add` round-trips `typhon.toml` deps.
- Strict `gather:` lowers to `asyncio.TaskGroup` and runs.
- `go ... -> task` lowers to `typhon_runtime.tasks.spawn` and runs.
- `model X:` emits clean `BaseModel` + `extra="forbid"` Pydantic class.

**The big breakage:** about a dozen documented features and rules don't
actually fire. The highest-leverage fixes by impact:

1. **Interface conformance only matches `class`-body methods, not `impl`
   blocks** (#26). The verbatim cheat-sheet example fails to compile.
   Cascades into bounded-generics (#27).
2. **`T?`-returning function cannot be bound to a `T?` annotation** (#7).
   Caused by a logic bug in `assignable` for Union/Union. Makes the
   documented nullable workflow basically unusable.
3. **`@pure` checks are wired into `tyc build` only**, not `tyc check`
   (#28). CI hole.
4. **Comptime evaluator supports a fraction of the documented surface**
   (#29) — no list/dict/tuple, no string method calls, no comparisons.
5. **Rule 1 ("every parameter and return type annotated") is not
   enforced** (#30) — `def f(x): return 1` type-checks clean.
6. **`class Foo frozen:` does not parse at all** (#1). `guard NAME =
   EXPR else: ...` does not parse at all (#2). `impl[T] X[T]:` does not
   parse at all (#3). Implicit-field reference inside `impl` does not
   resolve (#10). All four are docs-promised features that hit
   `tyc::parse` or `tyc::unknown_name`.
7. **`tyc fmt` is a no-op** beyond whitespace cleanup (#18).
8. **Float literal `1.0` emits as `1`** (#31) — visible in JSON output,
   `isinstance(x, float)`, etc.
9. **Self-referencing class annotations crash at import** (#32) — any
   binary-op overload (`__add__`, `__lt__`) blows up `NameError`.

**Doc drift to clean up:** four diagnostic codes renamed without updating
docs (#36); `lazy import` says `LazyLoader` but emits a bespoke proxy
(#16); `gather:` bindings are an implicit exception to the `let`/`mut`
rule with no docs note (#5); `dict.get(k)` typed as `V` not `V?` (#11).

**Critical / blocker bugs** (block headline-feature happy-paths): #1, #2,
#3, #7, #10, #18, #26, #27, #28, #29, #30, #31, #32.

**Roughly-ranked fix order for a v0.2 push:**

1. **#7** — `assignable` Union/Union. One-line fix
   (`if expected == actual { return true; }` at the top of
   `tyc/crates/tyc-types/src/lib.rs:177`). Unblocks every nullable
   binding.
2. **#26 / #27** — interface conformance. Pull `impl`-block methods into
   the conformance scan (likely in `tyc-types`'
   `class_conforms_to_interface`).
3. **#30** — missing-annotation enforcement. Fail closed when
   `no-implicit-any = true`. The `assignable` arm
   `(Type::Unknown, _) | (_, Type::Unknown) => true` is the smoking gun.
4. **#31** — float literal. One-line fix in
   `tyc-emit/src/printer.rs:909` (use `{:?}` or detect whole numbers
   and append `.0`).
5. **#32** — self-ref annotations. Emit `from __future__ import
   annotations` at the top of every output module.
6. **#1, #2, #3** — parser gaps. Preprocessor needs three new shapes:
   `class NAME frozen:`, `guard NAME = EXPR else: ...`, and
   `impl[T1, T2] X[T1, T2]:`.
7. **#28** — check/build feature parity. Call `analyse_purity` and
   `evaluate_comptime` from `tyc/src/commands/check.rs`.
8. **#10** — implicit field reference inside `impl`. Less critical given
   `self.NAME` works; deciding to deprecate the implicit form is also a
   valid resolution.
9. **#29** — comptime evaluator scope. Either expand or shrink the docs.
10. **#18** — `tyc fmt`. Set expectations in docs since AST-based
    formatter was already deferred per `tyc-format/src/lib.rs:17`.

The rest (#4, #5, #8, #11, #12, #13, #14, #15, #16, #17, #19, #20,
#21, #22, #23, #24, #25, #33, #34, #35, #36) are papercuts and doc
fixes.

---

## 1. `class Foo frozen:` is a parse error (bug)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Mirrored the existing
`class!` raw-class machinery: the preprocessor now strips the `frozen`
modifier in `strip_frozen_modifier`, records the line in
`PreprocessResult::frozen_class_lines`, and the desugar pass emits
`@dataclasses.dataclass(slots=True, frozen=True)` (via
`make_dataclasses_dot_dataclass_decorator_frozen`) for any class whose
source range covers a recorded offset. Verified end-to-end: `class Point
frozen:` parses, builds, and mutation raises `FrozenInstanceError` at
runtime.

**Severity:** bug — documented feature is unparseable.

Both `docs/guides/` examples and the skill cheat sheet say:

```ty
class Point frozen:
    x: float
    y: float
```

Reality:

```text
× parse error in 'src/04_classes.ty'
  ╭─[src/04_classes.ty:14:13]
14 │ class Point frozen:
   ·             ▲
   ·             ╰── Expected `:`, found name at byte range 263..269
```

The parser does not recognise the `frozen` class modifier at all. Same error
for `class Frozen frozen:` in `05_class_violations.ty`. This blocks every
`frozen` test path. See `tyc-syntax` / vendored `ruff_python_parser`; the
modifier must be recognised before the colon.

---

## 2. `guard NAME = EXPR else: BODY` is a parse error (bug)

**Severity:** bug — documented core feature is unparseable.

Both the readability section and the pitfalls list show:

```ty
guard w = weight else: return
```

Reality:

```text
× parse error in 'src/10_unsafe_pipes.ty'
  ╭─[src/10_unsafe_pipes.ty:20:11]
20 │     guard w = weight else: return
   ·           ▲
   ·           ╰── Simple statements must be separated by newlines or semicolons
```

Same on the single-line form `guard r = find(1) else: return`. `guard` is
either entirely missing from the grammar or only the multi-line form
(`else:\n    return`) parses — needs investigation in `tyc-syntax`.

---

## 3. `impl[T] Box[T]:` parameter-list parses fail (bug)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Extended the
preprocessor's `impl` recognition to also match `impl[` (no space) and
taught `make_impl_class_line` to peel the leading `[T, U]` type-param
list, drop the trailing `[T, U]` application on the class name, and
forward the type parameters onto the pseudo-class header
(`class __typhon_impl_Box[T](object):`). Methods inside the block can now
resolve `T`/`U`; the desugar pass continues to merge them back into the
real `class Box[T]:`. (Note: `Box[int]` constructor inference at the
call site is a separate type-checker enhancement not yet on the
roadmap.)

**Severity:** bug — generic `impl` block syntax doesn't parse.

Docs explicitly show:

```ty
impl[T] Box[T]:
    def map[U](f: Callable[[T], U]) -> Box[U]:
        return Box(value=f(value))
```

Reality:

```text
× parse error in 'src/08_generics.ty'
  ╭─[src/08_generics.ty:14:9]
14 │ impl[T] Box[T]:
   ·         ▲
   ·         ╰── Simple statements must be separated by newlines or semicolons
```

Without this, no generic class can carry generic methods. Hard blocker for
the "PEP 695 only" generics story.

---

## 4. `gather(strategy="best-effort")` lowering breaks scope (bug)

**Severity:** bug — emitter produces code the resolver rejects.

Source:

```ty
async def gather_best_effort(uid: int) -> None:
    gather(strategy="best-effort"):
        a = fetch(uid)
        b = fetch_b(uid)
    print(a, b)
```

Diagnostic — `tyc` reports an error *in its own desugared output*:

```text
× cannot find 'a' in scope
  ╭─[src/09_async.ty:23:5]
22 │     __typhon_gather_3__ = await asyncio.gather(...)
23 │     a, b = __typhon_gather_3__
   ·     ┬
   ·     ╰── not found in scope
```

Two issues here:

1. The desugarer for the best-effort `gather` emits a tuple-destructuring
   assignment without `let`/`mut`, so the resolver flags `a, b` as
   undeclared even though the user-visible source is correct.
2. The diagnostic is shown in terms of the *lowered* source line numbers
   (`asyncio.gather(...)` does not appear in the user's `.ty` file) — the
   span has leaked from `tyc-desugar` into `tyc-resolve` without being
   remapped back through the source map. Confusing for users.

Same scope-error pattern likely applies to the bindings produced by the
non-best-effort `gather:` too (need to confirm — see Finding #5 below).

---

## 5. `gather:` block syntax is undocumented at the binding-keyword level

**Status:** **FIXED** on `claude/update-findings-IdfrH`. The skill's
`gather:` section now explicitly notes that bindings inside the block
are an intentional exception to Rule 2 (no `let`/`mut` required —
the keyword itself introduces them as immutable single-assignment
names). The best-effort scope bug (Finding #4) remains open.

**Severity:** papercut — ambiguity in the docs.

Source `gather:` block uses plain `a = fetch(uid)` style — no `let`/`mut`.
The docs/skill show exactly this form, but Rule 2 ("locals require
`let`/`mut`") would otherwise reject it. It works for the strict-cancel
`gather:` (emits an inner `TaskGroup` block), but breaks for the
best-effort variant (Finding #4). Suggest documenting that `gather:`
bindings are an exception to Rule 2 — or change the syntax to require
`let`.

---

## 7. `T?`-returning function cannot be bound to `T?` annotation (bug, critical)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Added a dedicated
`(Union, Union)` arm in `assignable` (tyc-types/src/lib.rs) that uses subset
semantics — every actual variant must be assignable to some expected variant
— so `int | None = int | None` succeeds. The single-Union arms below it are
unchanged and continue to handle the asymmetric cases.

**Severity:** bug — critical; breaks the documented happy path for nullables.

```ty
def get() -> int?:
    return None

def main() -> None:
    let x: int? = get()    # SHOULD WORK
    print(x)
```

Reality:

```text
× type mismatch: expected `int | None`, found `int | None`
```

Same problem with `let x: int | None = get()`. Identical types are flagged
incompatible.

Root cause (located in `tyc/crates/tyc-types/src/lib.rs:177` —
`pub fn assignable`):

```rust
(Type::Union(variants), other) => variants.iter().any(|v| assignable(v, other)),
(other, Type::Union(variants)) => variants.iter().all(|v| assignable(other, v)),
```

When **both** `expected` and `actual` are `Union(int, None)`, the first arm
fires for the expected union and recurses with each variant against the
*actual* union. The recursion then hits the second arm (actual is a union),
which requires every actual variant be assignable to `int` (or `None`). It
never holds, so the equality fails.

A minimal fix is to short-circuit `expected == actual` before the union
arms, or check `Union == Union` set-equality first:

```rust
if expected == actual { return true; }
```

Knock-on: the *generic* `first[T](xs: list[T]) -> T?` case also fails for
the same reason (Finding #11 chains off this). Every bidirectional T? flow
through a typed binding breaks.

**Workaround:** drop the annotation (`let x = get()`) — but that loses
the type safety that motivated the annotation in the first place.

---

## 8. `nullable_use` and `type_mismatch` double-fire for the same call (papercut)

**Severity:** papercut — noisy diagnostics.

```ty
def find() -> str?: ...
def greet(name: str) -> None: ...

greet(find())
```

Three errors are reported for a *single* call:

1. `tyc::type_mismatch` "expected `str | None`, found `str | None`" on the
   `let raw: str? = find()` line (from Finding #7).
2. `tyc::type_mismatch` "expected `str`, found `str | None`" on the call.
3. `tyc::nullable_use` "possibly-None value used where `str` is required"
   on the same call.

#2 and #3 are the same underlying issue. Pick one — `nullable_use` is the
more helpful diagnostic because its help text mentions `is not None`
guarding — and suppress the other when it applies.

---

## 9. `tyc::pure_violation` is documented but never emitted (gap, critical)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. The diagnostic
existed under the name `tyc::impure_pure_fn` (#36 corrected the doc drift)
and was already wired into `tyc build`. The CI hole was that `tyc check`
skipped the purity pass entirely — fixed by #28, which calls
`analyse_purity` + `purity_diagnostics` from `tyc check`. All four
examples in the original finding now produce `tyc::impure_pure_fn`
errors under both `tyc check` and `tyc build`.

**Severity:** gap — documented language feature is a no-op.

The `@pure` decorator is the canonical contract in Typhon — six conditions
the analyser is supposed to enforce. Every condition fails silently:

```ty
@pure
def naughty(s: str) -> None:
    print(s)             # I/O — should be tyc::pure_violation

@pure
def uses_time() -> float:
    import time
    return time.time()   # nondet — should be tyc::pure_violation

@pure
def raises_exc(n: int) -> int:
    raise ValueError("x")  # exceptions — should be tyc::pure_violation

mut GLOBAL: int = 0

@pure
def touches_global(n: int) -> int:
    GLOBAL = GLOBAL + 1   # mutates module state — should be tyc::pure_violation
    return n
```

All four type-check clean. `rg "pure_violation" tyc/` returns nothing —
the diagnostic does not exist in source. The `tyc-analyse/src/lib.rs`
doc-comment explicitly describes "Phase 3 adds purity inference", but the
verification half is unimplemented.

Practical impact: anyone relying on `@pure` for safety is silently lied to.
Pair `@memo` with `@pure` and you can cache a function that calls
`time.time()`, blowing up the cache semantics.

---

## 10. Implicit field reference inside `impl` blocks does not work (bug)

**Severity:** bug — documented core feature does not resolve.

The skill cheat sheet and `docs/guides/05-classes-and-models.md` both say:

```ty
impl User:
    def display() -> str:        # no `self`
        return f"{name} (#{id})"  # fields as plain names
```

Reality:

```text
× cannot find 'name' in scope
× cannot find 'id' in scope
```

The implicit-`self` desugaring rewrites the parameter list but does not
introduce field bindings into the function's scope. Either the desugar
needs to insert `self.NAME` rewrites for every field-shaped name, or the
docs need to retract "reference fields as plain names" and require `self`.

Worth deciding deliberately: removing the implicit form would simplify the
implementation a lot but reduce the "Rust-ish elegance" the docs are
selling. Keeping it would need a real resolve-time pass that knows the
enclosing class's field set.

Workaround: write `def display(self) -> str` and use `self.name` /
`self.id`. Explicit `self` parses, type-checks, and emits cleanly. That
makes the implicit-field story a pure UX nicety, not a feature anyone
actually relies on today.

---

## 11. `dict.get(k)` does not return `V?` (bug)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Added a small
`builtin_generic_method` lookup in `tyc-types`'s `Expr::Attribute`
resolution: when the receiver type is `Type::Generic("dict", [K, V])`
and the attr is `"get"`, the resolver synthesises a variadic
`Function { params: [K], ret: V | None }` signature. `let x: int =
d.get("a")` now correctly fails with the standard `type_mismatch`
diagnostic; `let x: int? = d.get("a")` succeeds and narrows correctly.
The helper is also the natural place to grow other built-in method
types (`list.pop`, `str.find`, `re.match`, etc.). Note: `d.get(k,
default)` is conservatively typed as `V | None` too — slightly stricter
than Python's runtime semantics, but never unsafe.

**Severity:** bug — type-stub drift.

```ty
let d: dict[str, int] = {"a": 1}
let x: int = d.get("a")       # should fail — d.get returns int?
print(x)
```

Type-checks clean. Either there's no stub for `dict.get`, or the stub
returns `V` instead of `V?`. Docs explicitly call this out as a pitfall:
"`dict.get(k)` typed as `V`. It's `V?`."

Worth a sweep of all the built-in container method signatures —
`list.pop`, `str.find` (returns `-1`, but `int?` would be cleaner once
`Optional` is correct), `bytes.find`, `re.match` (returns `Match?`), etc.

---

## 12. `gather:` (strict, no strategy) works correctly (positive note)

**Severity:** none — documenting what works.

```ty
async def f() -> None:
    gather:
        a = fa()
        b = fb()
    print(a, b)
```

Type-checks and emits to `asyncio.TaskGroup` correctly. The bug (Finding
#4) is specific to the `strategy="best-effort"` variant.

---

## 13. `tyc::result_error_mismatch` is documented but never emitted (gap)

**Severity:** gap — documented checker rule unimplemented.

```ty
def parse_port(raw: str) -> Result[int, str]:
    return Ok(int(raw))

def bad() -> Result[int, int]:        # E type is `int`
    let n: int = parse_port("80")?    # propagates Err[str]
    return Ok(n)
```

Should be `tyc::result_error_mismatch` per the diagnostics catalog. Reality:
type-checks clean. Means the `?` operator currently coerces error types,
defeating the safety story for typed errors.

---

## 14. `if x:` does not narrow `T?` to `T` (gap)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Added truthy
narrowing in `collect_narrowings_inner`'s `Expr::Name` arm: inside the
true branch of `if x:`, `x` is stripped of `None` (truthy implies
non-None). The else branch is intentionally not narrowed in the
opposite direction because falsy doesn't imply None — `int?` with
value `0` is falsy yet still nullable. Documented in the diagnostic
catalog as one of the supported narrowing forms.

**Severity:** gap — narrowing form unsupported.

Docs in `01-hello-world.md` / `02-values-and-types.md` and the skill list
`is None`, `is not None`, `isinstance`, `guard`, and early-return as
narrowing forms. Truthy narrowing (`if x:`) is not on that list. Confirmed:

```ty
let raw: str? = find(1)
if raw:
    greet(raw)            # tyc::nullable_use — narrowing failed
```

Either add truthy/falsy narrowing (with an obvious carve-out for `int?`
where `0` is truthy-false), or document explicitly that *only* the forms
above narrow. Today's behaviour is correct-by-design but undocumented.

---

## 15. `lazy import X = Y` is flagged as unused (bug)

**Severity:** bug — documented form falsely reported as unused.

```ty
lazy import np = numpy

def main() -> None:
    print(np)        # np is clearly used
```

Reality:

```text
× imported name 'np' is never used
  ╭─[t.ty:3:8]
3 │ import numpy as np
  ·        ─┬
  ·         ╰── imported here but never used
```

The diagnostic's span even shows the *post-preprocess* line
(`import numpy as np`), so the preprocessor has already rewritten `lazy
import np = numpy` before the unused-import pass runs. The unused-import
pass then doesn't see the lazy proxy class that actually receives the
`np` name — so the *real* binding is lost.

Worth deciding whether `lazy import` should be exempt from the unused-import
lint (since the proxy is always materialised, even when unused), or whether
the lint should track the post-lazy-rewrite name.

The emitted Python (when the file is otherwise clean) does correctly
generate the bespoke `__TyphonLazy_np_` proxy class.

---

## 16. `lazy import` docs say `LazyLoader`, emit uses a custom proxy (doc)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Replaced every
`importlib.util.LazyLoader` mention across `SKILL.md`,
`docs/language.md`, and `docs/guides/10-advanced-features.md` with a
description of the bespoke `__TyphonLazy_<alias>_` proxy that the
emitter actually produces (thread-safe via double-checked locking).
`REFERENCE.md` already showed the correct lowering. The
long-term-plan.md historical wording ("built on `importlib.util.LazyLoader`")
is left as-is because that doc records the *design intent* of an older
phase, not the current implementation.

**Severity:** doc — the implementation is fine, the docs are out of date.

The skill and several guides say:

> `lazy import name = module` lowers to `importlib.util.LazyLoader`.

Reality: emits a bespoke `__TyphonLazy_<name>_` proxy class with
double-checked locking via `threading.Lock`. The custom class is arguably
better (thread-safe; handles `__dir__`/`__repr__`; no LazyLoader's known
issues with `from`/relative imports). Just update the docs.

---

## 17. `class Foo:` body cannot contain `def` (correct, but only for impl)

**Severity:** positive note — emitting `tyc::method_in_class` would help.

When users put a method inside `class` (Python habit), they get an obscure
error rather than a guided diagnostic. The cheat sheet's pitfall #4 calls
this out, but there's no specific diagnostic — the user just gets a
type-system reject several layers deep. A targeted
`tyc::method_in_class_body` ("methods live in `impl <Class>:` — see [link]")
would close the loop.

---

## 18. `tyc fmt` is a no-op (bug)

**Severity:** bug — major UX feature unimplemented despite advertised.

```ty
def    f(  x:int,y:int)->int   :
        let    z:int=x+y
        return z
```

```text
$ tyc fmt messy.ty
0 files reformatted, 1 unchanged
```

Source is left untouched. The docs claim a "Typhon-aware printer wrapped
in ruff format". Either nothing happens, or the formatter only handles
clean files (in which case zero value).

Need to check whether the issue is preprocess-then-ruff-then-restore where
the restore drops formatting changes, or whether `tyc-format` is purely
skeletal. Worth a look in `tyc-format/src/`.

---

## 19. `interface` body cannot use declaration-only methods (papercut)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. The preprocessor
now recognises `def NAME(...) -> TYPE` lines that lack a body (no
trailing `:` after the return annotation) and auto-appends `: ...` so
the Python parser accepts them. The detector tracks paren/bracket depth
and strips trailing comments to avoid false positives. This makes the
documented `interface` syntax (`def draw() -> None`, no body) work
verbatim from the cheat sheet without losing the explicit `def f() ->
T: ...` form.

**Severity:** papercut — docs show forbidden form.

The skill (and `language.md`) show:

```ty
interface Drawable:
    def draw() -> None       # no body
    def width() -> float
```

Reality: this is a parse error ("Expected `:`, found newline"). Workaround
is `def draw() -> None: ...`. The parser inherits Python's "must have body"
rule from Ruff. Either preprocess `def NAME(args) -> T<NEWLINE>` inside
`interface` blocks to add `: ...`, or update the docs.

---

## 20. Best-effort `gather:` lowering produces bad source spans (papercut)

**Severity:** papercut — diagnostic shows lowered Python, not original Typhon.

See Finding #4 — the same emitted code that breaks resolve also produces
spans pointing at `asyncio.gather(..., return_exceptions=True)`, code the
user never wrote. The `.py.map` exists; the resolver/type-checker need to
consult it (or the desugarer needs to attach the original `.ty` span to
the synthesised assignment).

---

## 21. Migrate doesn't remove now-unused `from typing import Optional` (papercut)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. `tyc migrate`
now strips `Optional` from any `from typing import …` line, dropping
the line entirely when nothing else remains. Wildcard (`*`) and
`as`-aliased imports are intentionally left alone — those need manual
review. The migrated source no longer trips `tyc check`'s
`unused_import` warning.

**Severity:** papercut — generated code emits unused-import diagnostic.

`tyc migrate src/x.py` rewrites `Optional[T]` → `T?` but leaves
`from typing import Optional` as a dead import. Running `tyc check` on the
migrated `.ty` then errors with `tyc::unused_import`. Should detect that
zero call-sites for `Optional` remain after rewrite and drop the import.

Same likely applies to `from dataclasses import dataclass` if the file
already imported it under a re-export.

---

## 22. Migrate doesn't infer `mut` for reassigned module-level bindings (gap)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. The migrator now
scans for `global NAME[, NAME, ...]` statements anywhere in the file
and seeds the `reassigned` set with those names. The per-line walk
extends to plain (unannotated) module-level assigns: when the LHS name
is in the reassigned set, `mut` is prepended (`counter = 0` →
`mut counter = 0`). Type annotations are still left to the user since
literal-based type inference would be heuristic.

**Severity:** gap — known limitation; worth a `--strict` mode.

```python
counter = 0

def bump() -> int:
    global counter
    counter = counter + 1
    return counter
```

Becomes:

```ty
counter = 0    # should be: mut counter: int = 0

def bump() -> int:
    global counter
    counter = counter + 1
    return counter
```

The migrator's own help notes "cannot infer let/mut inside function bodies",
but **module-level** with a `global counter` + `counter = ...` write inside
a function is statically detectable. Mark it `mut`.

---

## 23. `Result[T, E]` pattern lowering converts tuple patterns to list patterns (bug, subtle)

**Severity:** bug — wrong semantically; works by accident in CPython.

Source:

```ty
match double_parse("a", "b"):
    case Ok((u1, u2)):
        ...
```

Lowered to:

```py
match double_parse("a", "b"):
    case Ok([u1, u2]):
        ...
```

The lowering turns the inner tuple pattern `(u1, u2)` into a list pattern
`[u1, u2]`. In CPython pattern matching, `[a, b]` is a sequence-pattern that
matches any sequence (list *or* tuple), so it happens to work — but it
also matches a literal `list`, which the user's `Ok((u1, u2))` would NOT.
Two issues:

1. `Ok(value=[1, 2])` and `Ok(value=(1, 2))` are now indistinguishable
   from the lowered pattern, even though the Typhon source distinguishes
   them.
2. The lowering is incorrect under strict pattern semantics; even if no
   user can name it today, the desugarer should preserve the exact
   pattern. Investigate `tyc-desugar`.

---

## 24. `typhon_runtime/__init__.py` uses PEP 695 `type` statement (gap)

**Severity:** gap — interacts with the "no runtime dep" pitch.

The generated `build/typhon_runtime/__init__.py` contains:

```py
type Result[T, E] = Ok[T] | Err[E]
```

This requires CPython **3.12+**. The README says Typhon emits "clean
CPython 3.13+" code and the project default is `[python] target = "3.13"`,
so this is consistent — but a few things need pinning down:

1. If a user sets `[python] target = "3.13"` and then ships the generated
   `build/` to a server running Python 3.11 (still on LTS in many
   ecosystems), the runtime won't load. The error is a confusing
   `SyntaxError: invalid syntax` on `type Result[T, E] = ...` rather than a
   "your interpreter is too old" diagnostic.
2. Lowering `type Result[T, E] = ...` to `Result = Ok | Err` (with explicit
   `TypeAlias`) would keep the file 3.10-loadable without breaking the
   types story. Worth a `[python] target = "3.10"` emit mode.

---

## 25. REPL does not auto-print expression statements (papercut)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Added a
`wrap_bare_expression_for_repl` text pass in `feed_block` that rewrites
single-line bare expressions (`>>> 1 + 1`) into `print(repr(...))`
before compiling. Conservatively skips any block that starts with a
keyword (`let`, `def`, `if`, ...), contains a top-level `=` other than
a comparison op, or spans multiple lines. Updated the module docstring
and the SKILL `tyc repl` quirks note to advertise the new behavior.

**Severity:** papercut — REPL UX gap.

```text
$ tyc repl
>>> 1 + 1
>>> let x: int = 5
>>> print(x)
5
>>>
```

`1 + 1` produces no output. The Python REPL would print `2`. The Typhon
REPL evaluates the statement but throws away the value. Add an implicit
`print(...)` for bare expression statements when running interactively.

---

## 26. Interface structural conformance breaks when impl is in `impl` block (bug, critical)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. The
`collect_classes_and_functions` pass in `tyc-types` now folds methods from
the `__typhon_impl_<Name>` pseudo-class (the preprocessor's lowering of
`impl Name:` / `extend Name:`) back into the target class's `InterfaceShape`
before interface conformance runs. The canonical Drawable cheat-sheet
example compiles, and bounded-generic conformance (#27) falls out
automatically.

**Severity:** bug — critical; canonical cheat-sheet example fails to compile.

Verbatim from the skill cheat-sheet:

```ty
interface Drawable:
    def draw() -> None: ...
    def width() -> float: ...

class Button:
    label: str

impl Button:
    def draw() -> None: print("x")
    def width() -> float: return 10.0

def render(d: Drawable) -> None:
    d.draw()

render(Button(label="x"))
```

Reality:

```text
× `Button` does not structurally conform to interface `Drawable`: missing or
│ incompatible member(s) draw, width
```

The interface conformance check **does not see methods defined in `impl`
blocks**. The structural-conformance pass appears to scan only the
methods defined *inside* the `class` body (the Python habit Typhon
explicitly tells you to avoid). The skill says (Rule 4):

> Methods live in `impl`, not in `class`

But conformance only works if you put them in `class`. Workaround:

```ty
class Button:
    label: str
    def draw(self) -> None: print(self.label)
    def width(self) -> float: return 10.0
```

…which is the form the docs tell you not to write.

Likely fix is in `tyc-types/src/lib.rs`'s `interface_missing_members` /
`class_conforms_to_interface` — they need to look up the merged
impl-block contributions from `tyc-desugar` (or the resolver) rather than
the raw class member list.

This is probably the highest-impact "core feature broken" bug in the
codebase.

---

## 27. `Drawable`-bounded generics reject conforming classes too (bug)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Same root cause as
#26; the impl-block merge in `collect_classes_and_functions` makes the
bounded-generic path see the impl-contributed methods.

**Severity:** bug — same root cause as Finding #26.

```ty
def render_all[T: Drawable](items: list[T]) -> None: ...

render_all(buttons)   # `Button` clearly satisfies, but rejected
```

```text
× type argument `Button` for `T` does not satisfy bound `Drawable`
```

Same root cause: interface conformance only matches in-class methods.
Once #26 is fixed this should fall out.

---

## 28. `tyc check` skips comptime evaluation and purity checks (gap, critical)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. `tyc check` now
calls `evaluate_comptime` and `analyse_purity` (via a `run_analysis_passes`
helper) for every checked `.ty` source after `check_file` runs. The LSP
path (`check_source_file`) is left as-is so editors don't flood users with
env-dependent errors while typing. CI pipelines that gate on `tyc check`
now catch `@pure` violations and missing required env vars instead of
deferring those failures to production builds.

**Severity:** gap — CI hole.

Both `comptime let` evaluation and `@pure` verification are wired into
`tyc build` but **not** `tyc check`:

- `cd tyc/crates/tyc/src/commands/check.rs` does not call `evaluate_comptime`
  or `analyse_purity`.
- `build.rs:215` calls `analyse_purity`; `build.rs:215`-ish also runs
  comptime.

Result: a CI pipeline that runs only `tyc check` accepts files that the
production `tyc build` rejects. Specifically:

- Required env vars (`[env] required`) are not verified by `tyc check`.
- `@pure` functions that do I/O or raise exceptions pass `tyc check`.
- Comptime expressions outside the narrow supported subset (Finding #29)
  are not caught.

`tyc check` is recommended as "the CI command" by the docs but is
genuinely weaker than `tyc build`. Either bring check up to parity, or
document loudly that `tyc build --check` is the real CI gate.

---

## 29. Comptime evaluator supports far less than documented (gap, big)

**Severity:** gap — docs over-promise; implementation does the minimum.

Skill / language.md says:

> The sandbox allows: pure arithmetic, string ops, env(name, default?),
> list/dict/tuple construction, calls to other comptime functions.

Reality (from `tyc-analyse/src/lib.rs`):

> Integer, float, string, and boolean literals.
> env("NAME") / env("NAME", "default").
> int(expr), str(expr), float(expr).

Tested unsupported forms:

```ty
comptime let A: int = 1 + 2 * 3          # OK
comptime let B: str = "hello".upper()    # FAILS — "only simple function calls"
comptime let C: list[int] = [1, 2, 3]    # FAILS — "expression is not a comptime-evaluable constant: List"
comptime let D: dict[str, int] = {"a": 1}# FAILS — Dict
comptime let E: tuple[int, str] = (1, "x") # FAILS — Tuple
comptime let F: bool = A > 5             # FAILS — Compare
comptime def feature(n: str) -> bool: ...# user-defined comptime fn — likely unsupported
```

Either expand the evaluator or shrink the docs.

---

## 30. Missing param / return types are not enforced (bug, critical)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Added a new
`tyc::missing_annotation` diagnostic and an `enforce_annotation_rule` pass
that runs from `check_function` (so the rule is enforced under both `tyc
check` and `tyc build`). Receivers (`self` / `cls`) are exempted, and
compiler-synthesised helpers named `__typhon_*` are excluded so desugar
bridges don't trigger user-facing errors. Methods inside `interface
Name:` bodies are unaffected because their preprocessed form already
declares `-> T` and takes no user-visible positional args.

**Severity:** bug — Rule 1 of Typhon is not enforced.

```ty
def f(x) -> int:      # missing param annotation; should be tyc::missing_return_type
    return 1

def g(x: int):        # missing return type; should be tyc::missing_return_type
    return x
```

Both type-check clean. Even with `[strictness] no-implicit-any = true`
in `typhon.toml`. The "every parameter and return type is annotated"
rule is the very first thing Typhon promises, and it doesn't fire.

`tyc-types` does have a per-call `Type::Unknown` fallback (see
`assignable` arm: `(Type::Unknown, _) | (_, Type::Unknown) => true`),
which is essentially what silently swallowing missing-annotation
violations looks like.

Affects the project's headline pitch: a "stricter superset of Python"
that doesn't enforce the strictness it advertises.

---

## 31. Float literals `1.0` emit as `1` (bug)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Switched
`tyc-emit/src/printer.rs`'s `Number::Float` arm from `{}` to `{:?}` so whole-
number f64 values keep their `.0` suffix in emitted Python.

**Severity:** bug — emit truncates whole-number floats.

```ty
let x: float = 1.0
let y: float = 0.0
let z: float = 2.5
```

Emits:

```py
x: float = 1     # !
y: float = 0     # !
z: float = 2.5   # OK
```

At runtime the value is still a Python `int`, not a `float`, which
breaks `isinstance(x, float)`, `repr(x)` ("1" not "1.0"), JSON
serialisation (`1` vs `1.0`), etc.

Root cause: `tyc-emit/src/printer.rs:909` does
`format!("{}", f)` for `Number::Float`. Rust's default `Display` for
`f64` drops `.0`. Either use `{:?}` (which always prints `.0`) or
detect the whole-number case and append `.0`.

---

## 32. Self-referencing type annotations break the emit (bug)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. `emit_mod` now
injects `from __future__ import annotations` at the top of every Python
build output (after any module docstring), so PEP 563 string-evaluation
makes self-references, recursive types, and operator overloads safe. The
header is only emitted in build mode (`suppress_mutability = true`) — `tyc
fmt` round-trips Typhon source unchanged.

**Severity:** bug — common pattern (operator overloading) blows up at import.

```ty
class Vec2:
    x: float
    y: float

impl Vec2:
    def __add__(self, other: Vec2) -> Vec2:
        return Vec2(x=self.x + other.x, y=self.y + other.y)
```

Emitted Python runs:

```text
NameError: name 'Vec2' is not defined
  File "...", line 9, in Vec2
    def __add__(self, other: Vec2) -> Vec2:
```

Python evaluates annotations at class-body time; `Vec2` isn't defined
yet. Fix: emit `from __future__ import annotations` at the top of every
generated module (PEP 563 string-evaluation), or detect self-references
and emit `"Vec2"` (string form).

Affects every binary operator overload (`__add__`, `__lt__`, `__eq__`),
every builder/factory method that returns `Self`, every recursive data
structure.

---

## 33. `let xs: list = []` shows a confusing "list vs list" diagnostic (papercut)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Taught `assignable`
that a bare built-in container annotation (`list`, `dict`, `tuple`,
`set`, `frozenset`, `deque`) accepts any parameterisation of the same
container — Python treats `list` and `list[Any]` interchangeably for
annotations, and the user's intent in `let xs: list = []` is clear.
`let xs: list = []` now type-checks; readers don't see the "expected
`list`, found `list[?]`" head-scratcher.

**Severity:** papercut — message text is misleading.

```ty
let xs: list = []
```

```text
× type mismatch: expected `list`, found `list[?]`
```

The expected and actual look identical except for the `[?]` marker. The
real complaint is "annotation is missing a type parameter", and the
diagnostic should be `tyc::implicit_any` ("the list element type is
implicit Any; annotate with `list[int]` or similar") rather than a
generic type mismatch.

---

## 34. `class Foo:` body method definitions silently bypass the impl-only rule (gap)

**Severity:** gap — Rule 4 not enforced.

```ty
class Button:
    label: str
    def draw(self) -> None:    # methods in class body
        print(self.label)
```

Type-checks clean. The docs explicitly say:

> Rule 4 — Methods live in `impl`, not in `class`. Writing methods
> inside `class` is `tyc::manual_init` / wrong-place errors.

…but the diagnostic does not fire. Worse, this is the **only** form
that satisfies interface structural conformance today (Finding #26),
so the rule and the conformance check pull in opposite directions.

Pick one:

- **Strict:** reject methods in `class`, force `impl`, and fix
  interface conformance to scan `impl`.
- **Permissive:** allow either, and fix interface conformance to scan
  both.

Right now we have the worst of both.

---

## 35. Doc-spec'd `auto-gather` requires undocumented `@gatherable` decorator (doc)

**Severity:** doc — opt-in works but the decorator's *existence* is
the gate, and it's barely described.

The skill mentions `@gatherable` once. Sleuthing the source confirms
`auto-gather` only rewrites `await callee()` runs when every callee
carries `@gatherable`. There is no diagnostic if you forget the
decorator — your awaits just don't get parallelised silently. A
`tyc::auto_gather_missed` info diagnostic ("two adjacent awaits look
independent; mark callees `@gatherable` to parallelise") would close
the loop.

---

## 36. Diagnostic-code drift between docs and reality (doc)

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Renamed the three
drifted codes across the skill (SKILL.md, REFERENCE.md, PITFALLS.md) and
the guides (`docs/guides/06-error-handling.md`,
`docs/guides/10-advanced-features.md`):
`tyc::let_reassign` → `tyc::immutable_assign`,
`tyc::result_propagate_outside_result` → `tyc::invalid_question_op`,
`tyc::pure_violation` → `tyc::impure_pure_fn`.

**Severity:** doc — diagnostic catalog in `docs/` and the skill drifts
from reality.

Several diagnostic codes do not match what `tyc` actually emits:

| Skill / docs | Actual |
|---|---|
| `tyc::let_reassign` | `tyc::immutable_assign` |
| `tyc::result_propagate_outside_result` | `tyc::invalid_question_op` |
| `tyc::pure_violation` | `tyc::impure_pure_fn` |
| `tyc::nullable_use` (in some pitfall examples; for `dict.get(k)` etc.) | also emitted as `tyc::type_mismatch` in many flows (Finding #8) |

These appear in the README, the diagnostics catalog inside the skill,
and `docs/guides/`. Pick one set of names and grep-fix the other —
the skill cheat-sheet table is the user-facing source of truth so
either rename the diagnostics in source, or rewrite the table.



**Severity:** doc — diagnostic catalog in `docs/` and the skill is wrong.

Several diagnostic codes do not match what `tyc` actually emits:

| Skill / docs | Actual |
|---|---|
| `tyc::let_reassign` | `tyc::immutable_assign` |
| `tyc::result_propagate_outside_result` | `tyc::invalid_question_op` |

These appear in the README, the diagnostics catalog inside the skill, and
likely in `docs/guides/`. Pick one set of names and grep-fix the other.

---

