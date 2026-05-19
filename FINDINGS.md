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

## Status as of `claude/update-findings-IdfrH`

Branch `claude/update-findings-IdfrH` ships fixes for all of the
above ranked items plus the long tail; each entry below has its own
**Status** block with implementation notes.

**Closed (33):** #1, #2 (single-line), #3, #4, #5, #7, #8, #9, #10
(docs path), #11, #13, #14, #16, #17, #19, #20, #21, #22, #23, #24,
#25, #26, #27, #28, #29 (docs path), #30, #31, #32, #33, #35, #36.

**Partially fixed (2):** #15 (false-positive resolved, span fidelity
deferred); #34 (warning added via #17; promotion to error is a
separate strictness-config decision).

**Still open (2):**
- **#15** lazy-import diagnostic span fidelity — the false-positive
  is gone, but when the diagnostic does fire its span points at the
  preprocessed `import X as Y` line rather than the user's `lazy
  import` source. Needs span remapping through preprocess metadata.
- **#18** `tyc fmt` is a no-op — needs an AST-based formatter; the
  Typhon-aware printer documented at `tyc-format/src/lib.rs:17` is a
  Phase-5 item, not in this branch's scope.

`cargo test --workspace --release` is green for every commit on the
branch.

---

## Status as of `claude/fix-findings-diagnostics-hYj8K`

This branch picks up the remaining open findings plus a handful of
polish follow-ups that were flagged in commit messages on the prior
branch but not implemented.

**Closed in this branch:**

- **#15** lazy-import span fidelity — `tyc::unused_import` on a
  `lazy import ALIAS = MODULE` line now renders the original
  Typhon source and anchors the label at the alias offset in the
  original text. Threaded through a new `ResolveOptions.lazy_import_remaps`
  + `original_source` pair, populated by `tyc-db`.
- **#2 (multi-line)** `guard NAME = EXPR else:\n    BODY` form now
  parses and lowers correctly through a new `expand_multiline_guards`
  pre-pass.
- **#34 (promotion)** `[strictness] methods-in-class-body = "error"`
  (also `"off"` / `"warn"`) routes through `apply_strictness` so
  projects can break CI on Rule-4 violations without changing the
  type-checker's default behaviour.
- **#13 polish** `tyc::result_error_mismatch` is now a dedicated
  diagnostic variant emitted at the `?`-op return site when the
  callee's `Err[E1]` doesn't match the caller's `Result[T, E2]`.
  The generic `tyc::type_mismatch` no longer absorbs this case.
- **#35 polish** `tyc::auto_gather_missed` advice-level diagnostic
  is now emitted by `tyc build` when `[strictness] auto-gather = true`
  and a run of independent adjacent awaits would have folded into a
  TaskGroup if every callee carried `@gatherable`.
- **#29 polish** Comptime evaluator now supports container literals
  (`[1, 2, 3]`, `{"a": 1}`, `(1, 2)`), pure string method calls
  (`.upper()`, `.split(",")`, etc.), and `len()` on str / list / tuple
  / dict. End-to-end verified through `tyc build`.
- **Rule-2 enforcement** `tyc::missing_binding_kind` now fires when
  a bareword `name = 1` appears as the first declaration of a name
  in a function/method scope. Synthesised `__typhon_*` temporaries
  are exempted. The `gather:` strict lowering now emits explicit
  `let user = ...` so its bindings flow through the same path
  without special-casing.

**Still open after this branch:**

- **#18** `tyc fmt` is a no-op — Phase-5 deferral, unchanged.

`cargo test --workspace --release` is green for every commit on this
branch.

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

**Status:** **FIXED** on `claude/fix-findings-diagnostics-hYj8K`.
The single-line form landed earlier; the multi-line block-body form
now also lowers correctly through a new `expand_multiline_guards`
pre-pass. The pass detects `<indent>guard NAME = EXPR else:` headers
with no body suffix, collects the strictly-deeper-indented body, and
emits the same three-statement shape as the single-line handler
(`let __typhon_mguard_<N> = (EXPR)`, `if __typhon_mguard_<N> is None:`,
body, `let NAME = __typhon_mguard_<N>`). Wired into every pipeline
call site (db check/build, repl, format) ahead of the other sugar
transforms so the body can still contain `?`, pipes, `gather:`, etc.

**Status (historic):** **FIXED (single-line)** on `claude/update-findings-IdfrH`. The
preprocessor now expands `guard NAME = EXPR else: BODY` into three
lines: a temp `let __typhon_guard_<line> = (EXPR)`, an `if … is None:
BODY` guard, and the user-facing `let NAME = __typhon_guard_<line>`.
The temp is necessary because the type-checker can only narrow `Name`
expressions, not arbitrary call results — without it
`guard u = find_user(t) else: …` would re-call `find_user` and lose
narrowing.

Paired with a new piece of flow-sensitive narrowing in `check_if`:
when the `if`-body always exits (return/raise/break/continue) and the
elif/else chain either is empty or also always exits, the negated
narrowing is applied to the post-`if` scope. This is what makes
`guard t = find_token(uid) else: return "anon"` correctly narrow `t`
for the rest of the function body, and is also what closes the
common idiomatic shape `if x is None: return; use_x_as_T()` for any
caller, not just `guard` expansions.

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

**Status:** **FIXED** on `claude/update-findings-IdfrH`. The real bug
turned out to be in `tyc-resolve`: `declare_target` only declared
names for `Expr::Name` targets, so a tuple-destructuring assignment
(`a, b = expr`) emitted by the best-effort `gather:` lowering left
`a` and `b` undeclared. Taught `declare_target` to recurse into
`Expr::Tuple` / `Expr::List` / `Expr::Starred` so destructured
bindings register properly. Both the strict and best-effort gather
forms now work end-to-end; the underlying improvement also makes
hand-written `let a, b = ...` style destructuring resolve correctly.
The follow-up span-fidelity concern (Finding #20) goes away because
the diagnostic no longer fires.

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

**Status:** **FIXED** on `claude/update-findings-IdfrH`. The arg-type loop
in `infer_expr_ctx`'s `Expr::Call` arm now branches: when the actual is
nullable and the parameter is not, it emits `nullable_use` exclusively
(skipping the redundant `type_mismatch` on the same span). The
fallback for non-name args (e.g. `greet(find())`) still emits
`type_mismatch` because there's no identifier to anchor a
`nullable_use` diagnostic at.

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

**Status:** **FIXED (docs)** on `claude/update-findings-IdfrH`. Took the
deprecation path suggested in the original finding: the SKILL.md
cheat-sheet, `docs/language.md`, and `docs/guides/05-classes-and-models.md`
were rewritten to use explicit `def display(self) -> str: return
self.NAME` instead of the previously-claimed bare-identifier form. A
history note in the guide records the deprecation so users coming from
older drafts know why their bare `name` references emit
`tyc::unknown_name`. Reintroducing the implicit sugar is a follow-up.

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

**Status:** **FIXED** on `claude/fix-findings-diagnostics-hYj8K`. The
polish follow-up from the prior branch now landed: a new
`TycError::ResultErrorMismatch` variant carries the dedicated
`tyc::result_error_mismatch` code, and the return-stmt type-checker
detects the synthesised `return __typhon_q_N__` form (produced by
`expand_question_ops`), extracts the `Err[E_callee]` and the
enclosing `Result[T, E_caller]`'s `E`, and emits the specific
diagnostic when those `E`s don't match. Falls back to the generic
`tyc::type_mismatch` when the shapes don't fit the `?`-op pattern.

**Status (historic):** **FIXED** on `claude/update-findings-IdfrH`. The root cause
was that `isinstance(x, Err)` narrowing reduced `x: Result[T, E]` to
the bare class `Class("Err")` — losing `E` — so the bare-`Result`
accepts-bare-class assignability arm forgave the mismatch when the
`?`-operator lowering re-emitted `return x` into a function with a
different `E`. Added a `refine_isinstance_target` helper that
preserves the generic parameter when narrowing `Result[T, E]`
against `Ok` / `Err`, giving the post-narrowing type
`Generic("Err", [E])`. The existing `Generic`-vs-`Generic` arm of
`assignable` then catches the mismatch: `Result[int, int]` rejects
`Err[str]` with the standard `tyc::type_mismatch` diagnostic.

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

**Status:** **FIXED** on `claude/fix-findings-diagnostics-hYj8K`. Span
fidelity now matches the user-written form: `ResolveOptions` carries
a `lazy_import_remaps: Vec<LazyImportRemap>` and the original Typhon
source, populated by `tyc-db` from `PreprocessResult::lazy_imports`.
When `report_unused_imports` is about to fire on an import binding,
the resolver checks whether the binding's preprocessed line index
matches a `lazy_import` line; if so, it swaps in the original
source and anchors the span at the alias offset in the original.
The diagnostic now reads `lazy import np = math` and points at `np`
instead of showing the preprocessor's `import math as np` rewrite.

**Status (historic):** **PARTIALLY FIXED** on `claude/update-findings-IdfrH`.
The headline false-positive was resolved on `main` (cannot be
reproduced today: `lazy import np = math` + a later `print(np)`
checks cleanly). The remaining issue was span fidelity, addressed
in the current branch.

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

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Added a new
`tyc::method_in_class_body` warning that fires when a `def NAME(...)`
appears inside a `class NAME:` body that isn't a pseudo-impl/extend
class, an interface (`Protocol`), or a Pydantic model. The diagnostic
includes both the class and method names and a help message pointing
at `impl ClassName:`. Dunders (`__add__`, `__lt__`, ...) are exempted
because operator overloads are a legitimate class-body use today.
Emitted as a warning so existing tests don't break; the FINDINGS doc
notes promotion to error is a separate v0.2 decision (#34).

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

**Status:** **FIXED (by #4)** on `claude/update-findings-IdfrH`. The
span-fidelity problem only existed because the resolver was emitting
a diagnostic on the lowered code. With #4's `declare_target` fix the
diagnostic no longer fires at all, so there's nothing whose span
needs remapping. If a future lowering reintroduces resolver-visible
synthetic code, the `.py.map` would need to be consulted by the
resolver and type-checker — a meaningful refactor.

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

**Status:** **FIXED (cosmetic)** on `claude/update-findings-IdfrH`. The
emitter's `Pattern::MatchSequence` arm now uses parens for 2+ element
sequences (keeping brackets for 0/1 element cases where `()` / `(a)`
would be ambiguous in pattern position). `case Ok((u1, u2))` now emits
verbatim instead of `case Ok([u1, u2])`. Worth noting that the
*semantics* were unchanged: per PEP 634, sequence patterns match both
list and tuple instances regardless of `[ ]` vs `( )` syntax, so the
"indistinguishable patterns" concern in the original finding was a
false alarm — the round-trip is now cosmetically clean too.

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

**Status:** **FIXED** on `claude/update-findings-IdfrH`. Replaced the
PEP 695 `type Result[T, E] = Ok[T] | Err[E]` line in the bundled
`typhon_runtime/__init__.py` template with the backward-compatible
`from typing import TypeAlias, Union; Result: TypeAlias = Union[Ok,
Err]` form. Generated build output now loads under Python 3.10 / 3.11
/ 3.12 as well as the 3.13+ default. The runtime never inspects the
alias's generic parameters at runtime, so dropping `[T, E]` is
harmless; static type checkers still see the union of `Ok` and `Err`.

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

**Status:** **FIXED** on `claude/fix-findings-diagnostics-hYj8K`. The
evaluator now supports the roadmapped surface that the docs had
to retract on the prior branch:
- container literals (`[1, 2, 3]`, `{"a": 1}`, `(1, 2)`, including
  empty containers and single-element tuples with the right trailing
  comma in the emitted Python),
- pure string method calls (`upper`, `lower`, `strip`, `lstrip`,
  `rstrip`, `replace`, `startswith`, `endswith`, `split`),
- `len()` on str / list / tuple / dict,
- chained method calls (`"  hi  ".strip().upper()`).

Unsupported methods produce a clear "not supported" diagnostic that
lists what IS supported instead of the generic "not comptime-evaluable"
message. End-to-end verified through `tyc build` — container literals
substitute as inlined Python literals (`TAGS: list[int] = [1, 2, 3]`),
and the emitted module runs unmodified.

**Status (historic):** **FIXED (docs)** on `claude/update-findings-IdfrH`. The
docs were tightened on the previous branch to match the evaluator's
narrow scope; the current branch expanded the evaluator instead.

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

**Status:** **FIXED** on `claude/fix-findings-diagnostics-hYj8K`. The
`[strictness] methods-in-class-body` config option now reroutes the
diagnostic in `apply_strictness`:
- `"warn"` (default) keeps the previous behaviour.
- `"error"` promotes the diagnostic so CI breaks on the form.
- `"off"` suppresses it entirely (useful for codebases mid-migration).

The type-checker still emits a single warning unconditionally; the
per-project policy decides how to render it. Promoting to error
unconditionally would have broken downstream users who rely on the
class-body form, so the toggle is the right escape hatch.

**Status (historic):** **PARTIALLY FIXED** on `claude/update-findings-IdfrH`. #17's
new `tyc::method_in_class_body` warning now flags every method
definition inside a non-pseudo, non-interface, non-Pydantic class
body. The diagnostic was a *warning* rather than an error; the
promotion config landed in the current branch.

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

**Status:** **FIXED** on `claude/fix-findings-diagnostics-hYj8K`. The
follow-up `tyc::auto_gather_missed` advice-level diagnostic now fires
from `tyc build` when `[strictness] auto-gather = true` and a run of
2+ adjacent independent `name = await CALLEE(args)` statements would
have been folded into a `TaskGroup` if every callee carried
`@gatherable`. Local async callees are eligible; imported async
callees are ignored (the user can't decorate them anyway). Printed
through miette so the rendered output shows the [Advice] severity
badge; doesn't block builds.

**Status (historic):** **FIXED (docs)** on `claude/update-findings-IdfrH`. The
docs were updated to call out the gate; the diagnostic itself
landed in the current branch.

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

## Status as of `claude/test-typhon-library-kxGRX` (fresh round, 2026-05-18)

Built `tyc` from this commit (release, ~84s clean build) and ran a new
battery of ~50 hand-written `.ty` programs through `tyc check`, `tyc build`,
`tyc fmt`, `tyc migrate`, `tyc repl`, `tyc add`, plus all 47 files in the
`examples/` suite. The compiler was inherited via the `main` merge after
the prior `claude/fix-findings-diagnostics-hYj8K` work landed.

**What's working well now:** the previously-fixed surface still holds —
`class Foo frozen:`, single-line `guard`, `impl[T] Box[T]:`,
`Union/Union` assign, `if x:` truthy narrow, `dict.get(k)` as `V?`,
`result_error_mismatch`, `method_in_class_body` warning, missing-annotation
enforcement (Rule 1), float-literal `1.0` emission, `from __future__
import annotations` injection, comptime container/string-method literals,
single-arg `extend BUILTIN:` end-to-end, simple `gather:` and `go`,
multi-module imports, recursive class refs, operator overloads, walrus,
list/dict comprehensions, `try/except` → `Result` bridging.

**The big new breakage:** Rule 2 enforcement (`tyc::missing_binding_kind`)
shipped without updating every desugarer to emit `let` — so several
documented headline features now fail at check time **on their own emitted
output**:

1. **`with` chain bindings (any form) emit `a = __typhon_with_N__.value`
   without `let`** — #37. Both single- and multi-binding forms break
   under `tyc check`. Critical, regression.
2. **Best-effort `gather:` emits `a, b = __typhon_gather_N__`** — #15 in
   the original list re-broken by Rule 2 enforcement. #38.
3. **`go fn() -> task` emits `t1 = typhon_runtime.tasks.spawn(...)`** —
   #39.
4. **For-loop tuple unpacking (`for k, v in d.items():`)** — the
   resolver doesn't register `k`/`v` as locals (declare-target doesn't
   recurse into `Expr::Tuple` for `for` targets) — #40. This blocks ~10
   of the 47 example programs single-handed.

**The other big new breakage** isn't a regression — these surfaced now
because the test surface grew:

5. **F-string format specs (and `!r`/`!s` conversions) are completely
   stripped** by the printer — `f"{n:03d}"` → `f"{n}"`. #41. Critical.
   Any logging/CLI tool that formats numbers loses its formatting.
6. **F-string with nested same-quoted strings emits raw-nested quotes**
   — `f"{'name':<10}"` → `f"{"name"}"`, breaking on 3.11 and confusing on
   3.13. #42.
7. **`Callable[[T], U]` cannot be called** — every value typed as
   `Callable` is rejected as not-a-function. #43. Blocks higher-order
   programming (`map`, `filter`, decorators, closures).
8. **Keyword args don't count toward arity, defaults don't either, `*args`
   defs report `expected 0`, trailing-comma-in-call reports `got 0`** —
   the arg-count check counts only positional, no-default, no-trailing-
   comma args. #44.
9. **Covariance not implemented** — `list[Drawable]` rejects
   `list[Button|Slider]`; `Result[Cmd, _]` rejects `Ok[SubCmd]` even
   when `SubCmd ⊆ Cmd`. #45.
10. **Generic class instantiation doesn't infer type params** —
    `Box(value=42)` infers `Box` (no params), making
    `let b: Box[int] = Box(value=42)` fail. #46.
11. **PEP 695 syntax emitted even when interpreter is < 3.12** — `def
    first[T](...)` and `type T = A | B` survive `from __future__ import
    annotations` (PEP 695 is syntactic, not annotation-string-based),
    so output fails with `SyntaxError` on the user's runtime. No
    diagnostic when `[python] target` ≥ 3.12 but interpreter is 3.11.
    #47.
12. **Comptime can't reference other comptime constants, can't call
    user-defined `comptime def`** — both are documented as supported.
    #48.
13. **`tyc::missing_await` not enforced** — sync function `let x: int
    = fetch()` (coroutine assigned to int annotation) checks clean. #49.
14. **`class` body `__init__` accepted; dataclass + manual `__init__`
    both emitted; documented as `tyc::manual_init`** — #50.
15. **`yield` in `-> int` (non-iterator) checks clean** — #51.
16. **Multi-line `|>` (newline at start of pipe segment) fails to parse**
    — common Python wrap pattern. #52.
17. **REPL `print(x)` prints `None` after the value** — auto-print pass
    wraps even bare calls and re-prints the result. #53.
18. **`extend BUILTIN:` emits the lifted free fn with implicit-`Any`
    `self`** — `__typhon_ext_str__slug(self) -> str:`. No type
    annotation on the parameter even though it's known to be `str`. #54.
19. **`tyc init NAME`** scaffolds into CWD, not into `./NAME/`. #55.
20. **Examples suite — 27 of 47 fail `tyc check`**. #56.

**Still open from prior branches:** #18 (`tyc fmt` no-op — confirmed).

**Roughly-ranked fix order for a v0.3 push:**

The Rule-2 desugar regressions (#37, #38, #39) and the for-loop unpack
(#40) should be fixed first — together they account for most of the
example-suite failures. The f-string printer (#41, #42), arg-count
counting (#44), and `Callable` invocation (#43) are next: these aren't
regressions but they each block whole feature areas (logging, CLI,
higher-order programming) that the examples depend on. After that:
covariance (#45) is the big design call — it unblocks the rest of the
examples but needs care around variance rules. Then the long tail.

`cargo test --workspace --release` was not re-run on this branch (no
code changes from me — this is a pure stress-test pass).

---

## 37. `with`-chain bindings emit without `let`, fail Rule 2 (bug, critical)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`. The
`render_chain` desugarer in `tyc-syntax/src/preprocess.rs` now prefixes
both the success unwrap (`let NAME = __typhon_with_N__.value`) and the
`else err:` error binding (`let err = __typhon_with_N__.error`) with
explicit `let` keywords so the resolver records them as immutable
locals. Verified end-to-end: the cheat-sheet `with x = f()?:` example
now `tyc check`s clean and produces runnable Python.

**Severity:** bug — critical; regression caused by Rule 2 enforcement
shipping in `claude/fix-findings-diagnostics-hYj8K` without updating
the `with`-chain desugarer.

```ty
def f1() -> Result[int, str]:
    return Ok(1)

def chain() -> Result[int, str]:
    with a = f1()?:
        return Ok(a)
```

```text
tyc::missing_binding_kind
  × local binding 'a' is missing `let` or `mut`
   ╭─[src/main.ty:8:5]
 7 │         return __typhon_with_0__
 8 │     a = __typhon_with_0__.value
   ·     ┬
   ·     ╰── declare with `let` or `mut`
```

The lowering emits `a = __typhon_with_0__.value` rather than `let a =
__typhon_with_0__.value`, and Rule 2 then rejects it. The diagnostic
even shows the lowered code (`return __typhon_with_0__`), so span
fidelity is also broken on the same path (cf. Finding #20).

Same root cause makes every `with`-chain in `examples/07-error-handling/`
fail. The desugarer for `with NAME = EXPR?:` needs to emit the binding
as `let NAME = ...`. Synthesised compiler temporaries (`__typhon_*`)
are already exempted from Rule 2; the user-visible `NAME` is not.

---

## 38. Best-effort `gather:` tuple destructure missing `let` (bug, critical)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`. The
best-effort `gather` lowering in `render_gather_block` no longer emits a
tuple destructure (`a, b = __typhon_gather_N__`); instead each binding
is indexed into its own `let NAME = __typhon_gather_N__[i]` statement.
This matches the strict-gather form's per-binding shape and side-steps
Rule-2's `tyc::missing_binding_kind` cleanly. Verified at runtime — the
example program now builds and prints `1 2`.

**Severity:** bug — same family as #37; Rule 2 catches the synthesised
destructure.

```ty
async def run() -> None:
    gather(strategy="best-effort"):
        a = fa()
        b = fb()
    print(a, b)
```

The lowering produces `a, b = __typhon_gather_N__` — no `let`. Original
Finding #4 fixed `declare_target` to recurse into `Expr::Tuple`, so the
resolver now sees `a` and `b`. But Rule-2's `tyc::missing_binding_kind`
fires *before* / *alongside* that check on the same statement. Need to
either emit `let a, b = ...` (preferred — match the strict-gather form
that does `let a = ...; let b = ...` per element), or exempt the
synthesised assign from Rule 2.

The strict (non-best-effort) `gather:` already works because its lowering
spells out each binding individually as `let` (verified — emits a
sequence of `let user = ...` lines), so the fix is to mirror that for
the best-effort path.

---

## 39. `go fn() -> task` emits without `let` (bug)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`. The
`expand_go_calls` pass now emits `let NAME = typhon_runtime.tasks.spawn(...)`
when a `-> handle` is supplied. The bare `go f(x)` form is unchanged
(no user-visible target to declare). Verified end-to-end via `tyc build`
and a runtime `await`.

**Severity:** bug — same family as #37/#38.

```ty
async def run() -> None:
    go work(1) -> t1
    let v: int = await t1
```

Lowers to `t1 = typhon_runtime.tasks.spawn(work(1))`. Rule 2 fires:

```text
tyc::missing_binding_kind
  × local binding 't1' is missing `let` or `mut`
```

Same fix family — emit `let t1 = typhon_runtime.tasks.spawn(...)`.

---

## 40. `for k, v in d.items():` doesn't declare `k`/`v` (bug, critical)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`. Extracted
a `declare_loop_target` helper in `tyc-resolve/src/lib.rs` that recurses
into `Expr::Tuple` / `Expr::List` / `Expr::Starred` and reuses it from
the `Stmt::For`, `Stmt::With`, and comprehension generator walkers. The
helper declares each contained name as `BindingKind::Loop` with
`Mutability::Mut`, the same shape used previously for bare-name loop
targets. Verified end-to-end on `for k, v in d.items()`,
`for i, x in enumerate(xs)`, `for (a, b) in pairs:`, and tuple-target
comprehensions.

**Severity:** bug — critical; basic Python idiom; cascades through the
examples suite.

```ty
def main() -> None:
    let xs: list[tuple[int, str]] = [(1, "a"), (2, "b")]
    for i, s in xs:
        print(i, s)
```

```text
tyc::unknown_name
  × cannot find 'i' in scope
  × cannot find 's' in scope
```

The resolver's `for`-target walk handles bare `Name` targets but doesn't
recurse into `Tuple` targets. Finding #4's fix added tuple recursion to
`declare_target` for general-assignment LHS but didn't apply the same
to the `for`-stmt target.

This single bug blocks `for k, v in dict.items():`, `for i, x in
enumerate(xs):`, `for (a, b) in pairs:`, and similar — present in at
least 10 of the 47 example programs (20-logging, 42-rag-system,
43-agent-framework, and others).

Workaround: `for pair in xs: let i: int = pair[0]; let s: str = pair[1]`
— awful.

---

## 41. F-string format specs and conversions are stripped (bug, critical)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`. The
`Expr::FString` arm in `tyc-emit/src/printer.rs` now walks each
`InterpolatedElement`'s `conversion` flag (`!r` / `!s` / `!a`) and
`format_spec` (a nested mini-f-string supporting interpolations like
`f"{n:>{width}}"`). Verified: `f"{n:03d}"` → `005`, `f"{n!r}"` → `5`,
`f"{pi:.2f}"` → `3.14`, `f"{s:>10}"` → `        hi`.

**Severity:** bug — critical; emitter loses information.

```ty
def main() -> None:
    let n: int = 5
    let s: str = "hi"
    print(f"{n:03d}")
    print(f"{s:>10}")
    print(f"{n!r}")
```

Emits:

```py
def main() -> None:
    n: int = 5
    s: str = "hi"
    print(f"{n}")
    print(f"{n}")    # !r conversion lost
    print(f"{s}")
```

All format-spec (`:03d`, `:>10`, `:.2f`, `:>10.3f`, etc.) and conversion
(`!r`, `!s`, `!a`) suffixes are dropped. Pi-format `f"{pi:.2f}"` becomes
`f"{pi}"` — wrong runtime value rendered.

Root cause is in the printer's f-string handler in `tyc-emit/src/printer.rs`
(likely `FormattedValue` / `ConversionFlag` / `format_spec` not being
walked).

This breaks essentially every program that prints numbers with
formatting, every log line that pads/aligns, every CLI tool that uses
`:>` or `:<` alignment. Trivially user-visible.

---

## 42. F-string nested same-quoted strings emit malformed Python (bug)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`. The
printer now (1) tracks an `fstring_quote_stack` so a `StringLiteral`
emitted inside an interpolation auto-flips to the opposite quote
character, and (2) calls `pick_fstring_outer_quote` to choose the outer
`"`/`'` delimiter based on the literals that appear in any
interpolation expression. `f"{'name':<10}={'value':>5}"` now emits
unchanged on the Typhon side and runs cleanly on Python 3.11 and 3.13.

**Severity:** bug — output fails to parse on 3.11; ambiguous on 3.12+.

```ty
def main() -> None:
    print(f"{'name':<10}={'value':>5}")
```

Emits:

```py
print(f"{"name"}={"value"}")
```

Two problems:

1. The format-spec was dropped (Finding #41).
2. The single quotes inside the f-string were rewritten as double
   quotes (matching the outer string delimiter), producing a Python
   3.11 `SyntaxError: f-string: expecting '}'`.

PEP 701 (Python 3.12+) does permit identical-quote nesting inside
f-strings, so the output is technically valid on 3.13. But losing
the nested-quote flip-flop machinery is a portability regression
relative to typical Black/ruff output.

Fix: walk the f-string parts; if an inner string would collide with
the outer delimiter, swap quotes (Black's strategy).

---

## 43. `Callable[[T], U]` is not callable (bug, critical)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`.
`type_from_annotation` in `tyc-types/src/lib.rs` now special-cases the
`Callable[[P1, P2, ...], R]` and `Callable[..., R]` shapes and produces
a `Type::Function { params, ret, variadic }` directly, so the call-site
arm in `infer_expr` accepts it. Verified with both function arguments
and `Callable`-returning composition patterns; the previous
`tyc::not_callable` diagnostic no longer fires.

**Severity:** bug — critical; blocks higher-order programming, custom
decorators, closures, callback patterns.

```ty
from typing import Callable

def apply(f: Callable[[int], int], v: int) -> int:
    return f(v)

def main() -> None:
    print(apply(lambda x: x * 2, 5))
```

```text
tyc::not_callable
  × `Callable[?, int]` is not callable
   ╭─[src/main.ty:5:12]
 5 │     return f(v)
   ·            ──┬─
   ·              ╰── this value is not a function
```

`Callable[[T], U]` is treated as `Callable[?, U]` (param-list lost) and
then rejected from being called. Any function-returning-function,
custom decorator, closure-returning-closure, or callback-taking-fn breaks.

Confirmed root cause is in `tyc-types`: the `Callable[...]` shape is
constructed without a real param-type list, so the callable arm of the
expression type-checker never matches. Examples `22-http-requests` (HOF
retry pattern), `43-agent-framework` (tool dispatch table), several
others fail on this.

---

## 44. Arg-count check ignores keyword args, defaults, *args, trailing commas (bug, critical)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`. Added a
new per-function `ArityInfo` sidecar (param names, min/max positional
counts, kw-only names + required subset, `**kwargs` flag) populated by
`arity_info_from_parameters` and stored on `Checker.function_arity_info`.
The call-site arity arm now routes through `check_arity_with_info`,
which:

1. Counts kwargs against the parameter-name list, with `**kwargs` as a
   catch-all.
2. Allows positional counts in `[min_positional, max_positional]`,
   where `max_positional = None` whenever `*args` is declared.
3. Detects positional/keyword conflicts on the same parameter.
4. Requires every non-defaulted positional and kw-only param to be
   supplied (by either form).

The `function_signature` helper now also sets `variadic = true` when
the function declares `*args`. Verified end-to-end on each of the five
shapes called out by the finding (kwargs, defaults, `*args`, splatted
positionals, trailing-comma kwargs).

**Severity:** bug — critical; every multi-arg call with kwargs/defaults
fails.

```ty
def greet(name: str, prefix: str = "Hi", n: int = 1) -> None: ...
greet("alice", n=3)              # × expected 3, got 1
greet()                          # × expected 3, got 0  (when all default)

def f(a: int, b: int = 10) -> int: ...
f(1)                             # × expected 2, got 1  (default ignored)

def variadic(*args: int) -> int: ...
variadic(1, 2, 3)                # × expected 0, got 3  (*args expected 0?!)

def add(a: int, b: int, c: int) -> int: ...
add(*xs)                         # × expected 3, got 1

def long_call(a: int, b: int, c: int) -> int: ...
long_call(a=1, b=2, c=3,)        # × expected 3, got 0   (trailing-comma kills it)
```

The arg-count check appears to count only positional, non-defaulted,
non-trailing-comma arguments at the call site, and treats `*args`
defs as zero-arity functions.

Five distinct bugs in the same check:

a. **Keyword args** (`f(name="x")`) — not counted, treated as 0 args
   to the arity check.
b. **Default params** — `def f(a, b=10)` → calling `f(1)` rejected.
c. **`*args` def** — accepted definition but call site count is "expected 0".
d. **`**kwargs` def** — likely similar (didn't fully isolate).
e. **Trailing comma** — `f(a, b, c,)` treated as 0 args.

The first two together (a, b) make any function with a default param
practically unusable. Cascades across ~5 example programs.

---

## 45. Generic covariance not implemented (bug)

**Status:** **PARTIALLY FIXED** on `claude/resolve-open-findings-d6EIV`.
Three cases were addressed:

1. **`Result[T, E] = Ok[V] / Err[V]` with sealed-union T/E** — extended
   `Checker::is_assignable` to recurse into the generic-pair arm using
   itself rather than the free `assignable`, so sealed-union and
   interface-conformance rules apply inside `Result`. The
   `Result[Cmd, str] = Ok(AddCmd(...))` example now type-checks.
2. **Heterogeneous container literals against an interface / union
   element annotation** — `Expr::List`, `Expr::Dict`, and `Expr::Set`
   inference now widens the element type to the expectation when every
   inferred element is `c.is_assignable(expected, ...)`. Skipped when
   the expectation is an unbound TypeVar so PEP 695 inference still
   sees the concrete arg types. `let xs: list[Drawable] = [Button(...),
   Slider(...)]` now passes.
3. **`object` as the universal supertype** — added a top-level rule in
   `assignable` that accepts any value as a `Class("object")`, matching
   Python's runtime hierarchy and letting `list[dict[str, object]]`
   accept `[{"name": "x"}]` style literals.

What remains in this finding is the unsafe-by-default *general*
covariance for mutable containers (`list[Sub] → list[Super]`) which is
deliberately invariant in mypy/pyright and would require a deeper
read/write distinction to do safely.

**Severity:** bug — multiple symptoms; design call.

```ty
interface Drawable: ...
class Button: ...
class Slider: ...

let xs: list[Drawable] = [Button(...), Slider(...)]    # rejected
```

```text
tyc::type_mismatch
  × type mismatch: expected `list[Drawable]`, found `list[Button | Slider]`
```

Same with:

```ty
def parse() -> Result[Cmd, str]:
    return Ok(AddCmd(...))    # rejected: Ok[AddCmd] vs Result[Cmd, str]
```

```ty
let m: list[dict[str, object]] = [{"name": "x"}, {"name": "y", "age": 10}]
# rejected because inner inferred dicts are dict[str, str] / dict[str, str|int]
# not widened to dict[str, object]
```

Generics are treated invariantly. There's a design call here — Python's
own typing treats `list[T]` invariant for type-checker safety, so this
isn't strictly wrong, but combined with sealed unions and `Result`
ergonomics the friction is severe. The most-leverage fix:

- **`Result[T, E]`**: variant-form widening on return — when the function's
  declared return is `Result[T, E]` and the expression is `Ok(v: V)` where
  `V ⊆ T`, accept. This is how all rust-y Result languages work.
- **Heterogeneous-list inference**: when a literal `[a, b, c]` is bound
  to `list[T]` and every element is `⊆ T`, infer the LHS instead of the
  joined RHS type. Same for dict/tuple.

Cascade: 17-file-io-json, 21-cli-tool, 25-sqlite-database, 09-interfaces.

---

## 46. Generic class instantiation drops type params (bug)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`. Added a
`class_type_params: HashMap<String, Vec<String>>` side table on
`Checker`, populated from each generic class's PEP 695 type-params at
shape-collection time. The constructor-call arm now branches on this
table: when the class is generic, it
1. Reads any LHS annotation (`let b: Box[int] = ...`) and pins each
   parameter from the matching position (bidirectional inference).
2. Walks the constructor's keyword arguments, matches each to the
   class's field annotation, and binds any `TypeVar` mentioned in the
   field type from the arg's inferred type
   (`bind_field_typevars` recurses through generics and unions).
3. Returns `Type::Generic(name, [bound_T_values])`, falling back to
   `Type::Unknown` for any param still unbound.

Verified with `let b: Box[int] = Box(value=42)` and the symmetric
`let s: Box[str] = Box(value="hi")` — both pass `tyc check`.
Non-generic classes still produce `Type::Class(name)` exactly as
before.

**Severity:** bug — generic types unusable through their constructors.

```ty
class Box[T]:
    value: T

impl[T] Box[T]:
    def unwrap(self) -> T:
        return self.value

let b: Box[int] = Box(value=42)
```

```text
tyc::type_mismatch
  × type mismatch: expected `Box[int]`, found `Box`
   ╭─[src/main.ty:9:23]
 9 │     let b: Box[int] = Box(value=42)
   ·                       ──────┬──────
   ·                             ╰── expected `Box[int]`
```

`Box(value=42)` should infer `Box[int]` from the field type. Today it
infers bare `Box` (no params). Either:

1. Bidirectional: when LHS is annotated `Box[int]`, propagate `T=int`
   to the constructor call and check `value: int`.
2. Forward: from `value: T` and arg `42`, solve `T = int`.

Both are standard. Workaround: drop the annotation — `let b = Box(value=42)`
— but you also lose `Box[int]` propagation to downstream sites.

---

## 47. PEP 695 syntax emitted even when interpreter is < 3.12 (gap)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`. The
emitter now lowers PEP 695 syntax for `[python] target` versions
`< 3.12`. The build pipeline parses the target version via the new
`parse_python_minor` helper and threads it through
`emit_python_with_line_offsets_for_target`. When lowering:

- A module-prelude scan collects every distinct `T` declared on any
  `def`, `class`, or `type` and emits `T = TypeVar("T")` plus the
  matching `from typing import TypeVar, Generic, TypeAlias` line.
- `def f[T](...)` emits without the `[T]` suffix — the function refers
  to the synthetic global `T` instead.
- `class Box[T]:` emits as `class Box(Generic[T]):` (preserving any
  existing bases).
- `type X = Y` emits as `X: TypeAlias = Y`.

Verified: `target = "3.11"` produces output that runs on Python 3.11;
`target = "3.13"` (the default) is unchanged and still emits PEP 695.
Generic functions, generic classes, and type aliases all round-trip.

**Severity:** gap — silent runtime failure; doc says "clean CPython 3.13+"
but no enforcement.

```ty
def first[T](xs: list[T]) -> T?:
    return xs[0] if xs else None

type Color = Red | Green | Blue
```

Emits unchanged:

```py
def first[T](xs: list[T]) -> T | None: ...
type Color = Red | Green | Blue
```

Both `def f[T](...)` (PEP 695 generics) and `type X = ...` (PEP 695 type
aliases) are **syntactic** features that need Python 3.12+. `from
__future__ import annotations` (which `tyc-emit` correctly injects)
doesn't help — that flag only affects annotation strings, not the
top-level grammar.

On Python 3.11 (still the default in many CI images, Ubuntu 22.04 LTS,
etc.) the emitted output fails with `SyntaxError` at import time. Two
mitigations:

1. **Detect interpreter mismatch**: if `[python] target = "3.13"` but the
   interpreter at `tyc build` time is older, warn loudly. (Today: no
   warning.)
2. **Lower for older targets**: when `[python] target = "3.10"` /
   `"3.11"`, lower `def f[T]:` to `TypeVar`-based form and `type X = ...`
   to `X: TypeAlias = ...`. Already done for `Result` in Finding #24's fix.

This bites every example that uses generics or sealed unions.

---

## 48. Comptime can't reference other comptime constants or call comptime defs (gap)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`. Two
fixes:

1. `evaluate_comptime_with_functions` now seeds each binding's
   `EvalContext.locals` with every previously-evaluated comptime
   constant in source order. `comptime let B: int = A + 10` resolves
   `A` from the prior `comptime let A: int = 5`.
2. `tyc check` now calls `evaluate_comptime_with_functions` with the
   preprocessor's `comptime_functions` list (was passing an empty
   slice via the old `evaluate_comptime` wrapper). `comptime def
   is_prod(name: str) -> bool: ...` is now dispatchable from
   `comptime let SHIPS: bool = is_prod("dev")` under both `check`
   and `build`.

Verified end-to-end on the finding's example program.

**Severity:** gap — documented features unimplemented.

```ty
comptime let A: int = 5
comptime let B: int = A + 10   # × unknown name 'A' in comptime expression

comptime def is_prod(name: str) -> bool:
    return name == "prod"
comptime let SHIPS: bool = is_prod("dev")   # × function 'is_prod' is not valid
```

Both errors are emitted by the comptime evaluator in `tyc-analyse`. The
skill explicitly documents:

> `comptime def feature(name: str) -> bool: ...`
> `comptime let SHIPS_AUTH: bool = feature("auth")`

…as a supported form. Today the sandbox scope only contains the
current expression's locally-bound names; module-level comptime constants
aren't seen, and user-defined `comptime def` functions aren't dispatched.

These limitations directly block `examples/15-comptime-config`.

Either:

- **Expand the evaluator** to (a) seed scope with previously-evaluated
  comptime constants from the same module, and (b) register
  `comptime def` bodies as callable.
- **Or shrink the docs** further; the prior round already pulled back
  the spec once, but `comptime def` and cross-`comptime let` references
  are still in the skill.

---

## 49. `tyc::missing_await` not enforced (gap)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`. Added the
infrastructure:

- New `TycError::MissingAwait` diagnostic variant with a
  `tyc::missing_await` code and a help string pointing at `await` /
  `asyncio.run`.
- Two new fields on `Checker`: `async_functions: HashSet<String>` (set
  during the same pass that populates `function_signatures`) and an
  `inside_await: u32` counter that the new `Expr::Await` arm bumps
  while inferring its operand.
- A `in_sync_function: bool` flag tracked through `check_function` so
  the check only fires inside `def` bodies — `async def` callers and
  module-level scope (`asyncio.run(coro())` entry-point pattern) are
  exempt.

The call-site arm now emits `tyc::missing_await` whenever the callee
resolves to a known async function name, the active scope is a sync
function body, and the call isn't directly under an `await`.

**Severity:** gap — documented hard error doesn't fire.

```ty
async def fetch() -> int:
    return 1

def caller() -> int:
    let x: int = fetch()    # should be tyc::missing_await
    return x
```

Type-checks clean. The skill says:

> A sync function calling an `async` one without `await` is a **hard
> error** (`tyc::missing_await`).

Reality: `fetch()` is treated as returning `int` (not `Coroutine[Any,
Any, int]`), so it binds happily to `let x: int = ...` and the caller
returns a coroutine to the print. At runtime Python emits the standard
"coroutine 'fetch' was never awaited" warning.

`grep -r "missing_await" tyc/crates/` should land in the async pass —
it's currently not wired.

---

## 50. `__init__` in `class` body silently accepted, both emitted (gap)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`. Added a
new `TycError::ManualInit` variant with the documented
`tyc::manual_init` code, plus the `manual_init` factory. The class-body
walk in `tyc-types/src/lib.rs` now intercepts a `def __init__(...)`
*before* the softer dunder skip, emits the error, and `continue`s past
the body — so the existing `method_in_class_body` warning doesn't
double-fire on the same span. The error fires at the function-name
span and the build cannot proceed, preventing the conflicting emitted
output described in the finding.

**Severity:** gap — Rule says it's an error; emitter produces wrong code.

```ty
class User:
    name: str
    def __init__(self, name: str) -> None:
        self.name = name
```

Type-checks clean (only emits the `method_in_class_body` warning;
`__init__` should be an error per docs). Emits:

```py
@dataclasses.dataclass(slots=True)
class User:
    name: str
    def __init__(self, name: str) -> None:
        self.name = name
```

This works at runtime (the user's `__init__` overrides the dataclass-
generated one), but it's wrong on three counts:

1. The skill says writing `__init__` is rejected with `tyc::manual_init`.
2. The emitter shouldn't emit a `@dataclass(slots=True)` decorator
   *and* a hand-written `__init__` — `slots=True` is incompatible with
   user-defined `__init__` in obscure ways (you have to use
   `object.__setattr__` to bypass the slots check).
3. Rule 4 says methods go in `impl`; this method goes in `class`. The
   `method_in_class_body` warning does fire but `__init__` should be a
   distinct, harder error.

---

## 51. `yield` in non-iterator-typed function checks clean (gap)

**Severity:** gap — type-checker accepts incorrect return-type.

```ty
def counter(n: int) -> int:
    mut i: int = 0
    while i < n:
        yield i
        i += 1
```

Type-checks clean. Should be `tyc::type_mismatch` or a dedicated
`tyc::generator_return_type` ("function with `yield` should be typed
`Iterator[T]` / `Generator[T, S, R]`").

At runtime calling `counter(3)` returns a `generator` object, not an
`int`, so any caller annotated `let x: int = counter(3)` would crash if
typed.

Async generators have the same issue (`async def gen() -> int: yield 1`
checks clean).

---

## 52. Multi-line pipe (`|>` at start of next line) fails to parse (bug)

**Severity:** bug — common Python wrap pattern.

```ty
let r: str = (
    "  hi  "
    |> str.strip()
    |> str.upper()
)
```

```text
tyc::parse
  × parse error in 'src/main.ty'
    ╭─[src/main.ty:10:10]
 10 │         |> strip_ws()
    ·          ▲
    ·          ╰── Expected an expression at byte range 167..168
```

Python normally allows operators at start of next line inside parens
(black/ruff format `+`, `and`, `|`, etc. that way). The `|>` operator
doesn't survive the same wrap — the lexer or preprocessor isn't seeing
the previous-line continuation. Same root cause in
`examples/10-pipes-and-guards`.

Single-line pipes work fine. Workaround: keep the chain on one line.

---

## 53. REPL re-prints `print(...)` return as `None` (bug, papercut)

**Severity:** papercut — REPL auto-print overzealous.

```text
$ tyc repl
>>> print(5 * 2)
10
None
>>>
```

The auto-print pass (Finding #25's fix) wraps too aggressively — it
rewrites `print(5 * 2)` into `print(repr(print(5 * 2)))`, which evaluates
the inner `print` (emits `10`) then prints the outer call's return
(`None`).

The fix in `feed_block`'s `wrap_bare_expression_for_repl` needs to detect
that the top-level expression is *itself* a call that produces visible
output (or just: detect a top-level `print(...)` call by name and skip
the wrap), or — simpler — only wrap when the bare expression's value is
non-None at type-check time (typed `None` returns are skipped).

Bare expressions otherwise work (`1 + 1` → `2`, `"hi".upper()` → `'HI'`).

---

## 54. `extend BUILTIN:` emits lifted free fn without param type annotation (papercut)

**Severity:** papercut — receiver type elided in lifted function.

```ty
extend str:
    def slug(self) -> str:
        return self.lower().replace(" ", "-")
```

Emits:

```py
def __typhon_ext_str__slug(self) -> str:    # self lacks type annotation
    return self.lower().replace(" ", "-")
```

The parameter `self` is unannotated, so:

- The lifted free fn technically violates Rule 1 (every param annotated).
- Downstream callers lose type-checking on the receiver position
  (the type-checker rewrites `s.slug()` to `__typhon_ext_str__slug(s)`
  using the receiver's static type, so this isn't a *runtime* bug —
  just a clarity gap).

Emit `self: str` (or the appropriate built-in type) for consistency.
Doesn't affect runtime; affects readability of generated code and
`tyc ty`'s second-opinion checking.

---

## 55. `tyc init NAME` scaffolds into CWD instead of `./NAME/` (papercut)

**Status:** **FIXED** on `claude/resolve-open-findings-d6EIV`. `tyc init` now
treats a positional `NAME` as the target sub-directory: `tyc init myapp`
creates `./myapp/typhon.toml`, `./myapp/src/main.ty`, `./myapp/tests/` and
prints `Initialised Typhon project 'myapp' in <abs>/myapp`. Bare `tyc init`
(no name) still scaffolds into the current directory and infers the name
from the basename, matching `tyc init --dir <existing>` behaviour. Tests
updated in `tyc/crates/tyc/src/commands/init.rs` and a new
`init_with_name_creates_subdirectory` regression guard verifies the parent
directory stays clean.

**Severity:** papercut — UX confusion.

```text
$ pwd
/home/user/playground
$ tyc init myapp
Initialised Typhon project `myapp` in /home/user/playground
  typhon.toml        ← created in CWD, not in playground/myapp/
  src/main.ty
  tests/
```

The recorded project name (`myapp`) is written into `typhon.toml` but
the project is scaffolded into the **current** directory. Every other
language scaffolder (`cargo new`, `npm init`, `bun init NAME`, `uv
init NAME`) creates a subdirectory. The CLI message also reads
ambiguously ("in /home/user/playground" vs "as /home/user/playground/myapp").

Likely fix in `tyc/src/commands/init.rs` — `mkdir NAME && cd NAME` then
write files. Document the breaking change in the next release notes.

---

## 56. Examples suite — 27 of 47 fail `tyc check` (gap)

**Severity:** gap — the shipped example suite doesn't pass the
shipped checker.

Running `tyc check` on every `examples/<NN>/<name>.ty` file in the
repo: 20 pass, 27 fail. Common root causes (each cascades through
multiple files):

| Root cause | Finding | Affected examples |
|---|---|---|
| `for k, v in d.items()` | #40 | 20, 42, 43, others |
| `Callable` not callable | #43 | 22, 43 |
| Arg-count w/ kwargs/defaults | #44 | 30, 38, others |
| Generic covariance | #45 | 09, 17, 21, 25 |
| `with`-chain missing `let` | #37 | 07 |
| Multi-line pipe | #52 | 10 |
| `let (x, y) = ...` parse | (subsumed by #46-family) | 04, 46 |
| Comptime user fn | #48 | 15 |

Fixing #37/#38/#39/#40/#43/#44/#45 would clear most of the suite.

The 20 examples that pass include `01-hello-world`, `02-variables-and-
types`, `03-control-flow`, `06-classes-and-models`, `11-string-
manipulation`, `12-math-operations`, `13-dates-and-times`, `14-regex`,
`16-file-io-text`, `18-file-io-csv`, `23-async-basics`,
`24-async-gather-and-go`, `29-numpy-arrays`, `33-pytorch-tensors`,
`41-llm-structured-output`.

A CI gate that runs `tyc check examples/**/*.ty` would catch
regressions in the language surface as the compiler evolves; today
the example suite is a documentation artefact rather than a test
corpus.

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

