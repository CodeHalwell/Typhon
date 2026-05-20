# Typhon — Stress-Test Findings (round 2026-05-20)

Date: 2026-05-20
Branch: `claude/test-typhon-library-ejNr5`
Compiler: `tyc 0.1.4`, built from this commit, `cargo build --release`.

This round generated ~140 new `.ty` programs against features the prior
rounds covered less deeply: walrus interaction, pipe lowering, `with`-chain
syntax variants, class-level constants, match exhaustiveness shapes, lazy
let scoping, type-checker coverage for operators / indices / unknown kwargs,
and verification of the previously-fixed findings. The repro corpus lives at
`stress/round-2026-05-20/cases/` (`build_one.sh <file>` to repro).

Numbering continues from the prior rounds (last #96 + the 2026-05-19 round).
Each finding starts with the round-local sequence `R3.N` so they can be
referenced without renumbering the global list.

## Executive summary

Roughly 140 cases run through `tyc build`. Of those:
- ~95 build and run correctly (happy paths).
- 16 verify previously-reported findings were genuinely fixed (#7, #11,
  #13, #14, #22, #26, #31, #32, #61, #62, #72, #77, #78, #79, #84, #92 —
  see the table below for case references). #67, #74, #93 are not
  retested in this round.
- ~25 surface fresh failures or regressions documented below.

**The new high-severity finds:**

1. **R3.1 (CRITICAL, silent)** — walrus operator parentheses are stripped at
   emission. `if (n := len(xs)) > 3:` becomes `if n := len(xs) > 3:` in the
   emitted Python; since Python's `:=` binds *very* loosely, `n` ends up bound
   to the boolean `len(xs) > 3` rather than to `len(xs)`. The test passes
   silently and produces wrong results. Affects every `while`/`if` walrus.
2. **R3.2 (CRITICAL, silent)** — class-level constants with defaults are
   destroyed by `@dataclass(slots=True)`. `class HTTP: GET: str = "GET"` makes
   `HTTP.GET` resolve at runtime to a `<member 'GET' of 'HTTP' objects>`
   slot descriptor, not `"GET"`. The natural Typhon pattern for "constants on
   a namespace" is broken with no diagnostic, and `match s: case HTTP.GET:`
   silently fails every match.
3. **R3.3 (bug)** — `with` chain bindings on a single line **without** an
   `else err:` clause are parsed as Python's regular `with` statement, and
   the `?` inside is rejected. With the `else err:` block, the same syntax
   parses correctly.
4. **R3.4 (bug, severe)** — `lazy import np = json` (lazy import **with
   alias**) compiles but crashes at runtime with `ValueError: module object
   for 'json' substituted in sys.modules during a lazy load`. Un-aliased
   `lazy import json` works.
5. **R3.5 (bug)** — `lazy let X: T = ...` inside an `impl` block desugars to
   a class-body `let X: T = self.NAME ...`, then fails type checking with
   `tyc::self_outside_impl`. The doc-promised lowering to `@cached_property`
   is missing.
6. **R3.6 (bug, type-checker gap)** — `s + n` for `s: str, n: int` is accepted
   silently; the runtime crashes with `TypeError`. Likewise `list + dict`.
   Operator type-checking is essentially absent for `+` (and presumably the
   other binary ops).
7. **R3.7 (bug, type-checker gap)** — `User(id=1, nmae="alice")` (wrong kwarg
   name) is accepted at `tyc check`/`build`; the constructor only fails at
   runtime. The class fields are statically known.
8. **R3.8 (bug, type-checker gap)** — `case Point(z=z):` against `class
   Point: x:int; y:int` is accepted silently and simply fails to match. No
   diagnostic that `z` isn't a Point field.
9. **R3.9 (bug)** — `let n: int = a / b` (truediv assigned to int) is
   accepted; the runtime value is a float `3.333...` placed into an
   `int`-annotated binding. `a / b` should be `float`.
10. **R3.10 (bug)** — `let z: bool = t[2]` where `t: tuple[int, str]` is
    accepted at static type-check time. The tuple arity is statically known.
11. **R3.11 (papercut)** — `c |> Counter.add(5)` desugars to
    `Counter.add(c, 5)` but the *type checker* reports
    `expected 1, got 2` because impl-method signatures are looked up without
    `self`. Either the pipe lowering needs to skip the arity check, or the
    impl-method lookup needs a flag for receiver-included calls.
12. **R3.12 (gap)** — exhaustiveness analyser doesn't recognise five
    legitimate "everything else" cases:
    - `case Point(x=x, y=y):` (keyword bindings = wildcard)
    - `case [*xs]:` (star sequence = wildcard for list)
    - guarded variant followed by an unguarded variant of the same class
      (`case Box(v=n) if n > 0:` / `case Box(v=n):`)
    - the `case _:` arm of a nested match
    - `case int() as n:` / `case list() as xs:` against a recursive type alias
      `IntTree = int | list[IntTree]`
    All five surface as `tyc::missing_return`, which is the wrong diagnostic
    because the function is actually total.
13. **R3.13 (regression check — STILL OPEN)** — finding #66 ("`?` cannot
    appear inside a sub-expression") confirmed still open: `Ok(add(parse_int(s)?, parse_int(t)?))`
    rejects.
14. **R3.14 (regression check — STILL OPEN)** — finding #83 ("`async def`
    with no `await` doesn't fire `async_without_await`") confirmed still
    open.
15. **R3.15 (regression check — STILL OPEN)** — finding #65 (`tyc fmt`
    near-no-op) confirmed: `let b:int=2`, `id : int`, `f(1,2)`, no spaces
    around `==`, all left unchanged.
16. **R3.16 (bug)** — `@classmethod` / `@staticmethod` inside `impl`
    blocks: calling `Counter.make(5)` for a `@staticmethod def make(n: int)`
    reports `expected 0, got 1` (the staticmethod arg count is read as 0).
17. **R3.17 (doc drift)** — the guides show `stubs/redis.dty` (file under
    `stubs/`), but the compiler **only** picks up `.dty` files inside the
    configured `src/` directory. `stubs/` is silently ignored.
18. **R3.18 (papercut)** — emitted `.pyi` stubs contain
    `@dataclasses.dataclass(slots=True)` decorators. Implementation
    decorators leak into the stub surface — most stub consumers expect just
    `class X:` with `...` bodies.

**Fixes confirmed working in this round (no regressions):**

| Old finding | What this round verified |
|---|---|
| #7 `T?` not assignable to `T?` | Both directions work (case 5, 65) |
| #11 `dict.get(k)` returns V (not V?) | Now returns V?; #138 rejects assignment to V |
| #13 `result_error_mismatch` never emits | Now emits cleanly (case 98) |
| #14 `if x:` doesn't narrow T? | Now narrows (case 99) |
| #22 migrate doesn't infer `mut` | Now does (case 96) |
| #26 interface conformance via `impl` | Works (case 38) |
| #28 `@pure` only on build | Works on build correctly (24, 25, 113, 114, 115) |
| #31 float literal 1.0 emits as 1 | Now emits `1.0` (case 76) |
| #32 self-ref class crashes | Now compiles, runs (case 77) |
| #50 `__init__` in class rejected | Behaviour intact |
| #61 `global` requires `let` | No longer required (case 58, 110) |
| #62 mutable defaults | Auto-wrapped in `field(default_factory=...)` (case 129) |
| #66 walrus binds inside `if` | Walrus binds, but `R3.1` precedence bug introduced |
| #67 `class X(TypedDict):` | (Not retested) |
| #72 bare `list` annotation | Rejected (case 78) |
| #74 typing.List etc. | (Not retested) |
| #77 class redeclaration | Rejected (case 80) |
| #78 `impl UnknownClass:` | Rejected (case 81) |
| #79 missing module | Rejected — multi-file project test |
| #84 lazy let runtime import | Works correctly (case 127) |
| #92 unused `def main()` | Now warns (multi-file test) |
| #93 inline comment in `gather:` | (Not retested directly, no breakage seen) |

---

## Findings — detail

### R3.1 — Walrus parens dropped at emission (CRITICAL, silent)

```python
def main() -> None:
    let xs: list[int] = [1, 2, 3, 4, 5]
    if (n := len(xs)) > 3:
        print(f"n={n}")
```

Repro: `cases/46_walrus_let.ty`.

Emitted Python:
```python
def main() -> None:
    xs: list[int] = [1, 2, 3, 4, 5]
    if n := len(xs) > 3:        # parens stripped
        print(f"n={n}")
```

Runtime output: `n=True`.

Python's `:=` has very low precedence: without the outer parens, `n` binds
to `len(xs) > 3` (a `bool`), not `len(xs)` (an `int`). Two distinct
breakages flow from this:

- The `n=True` print is the user-visible smoking gun for this script, but
  in a realistic loop like `while (chunk := input()) != "":` (`cases/56`)
  the lowered `while chunk := input() != "":` becomes
  `while chunk := (input() != ""):` — `chunk` is now a `bool` and the loop
  condition is now `bool != ""` which is always truthy until input is `""`.
  The loop runs forever on empty input.
- The Typhon `let n: int` annotation that the type checker saw never
  reaches the emitted Python; only the walrus form does, and it's the
  walrus form that gets miscompiled.

Fix lives in the printer; binding-expression printing needs to retain the
parentheses (or know it needs them based on surrounding precedence).

### R3.2 — Class field defaults destroyed by `slots=True` (CRITICAL, silent)

```python
class HTTP:
    GET: str = "GET"
    POST: str = "POST"

def main() -> None:
    print(HTTP.GET)
```

Repro: `cases/121_class_attr_constant.ty`, `cases/126_class_const_methodref.ty`.

Emitted Python is `@dataclasses.dataclass(slots=True) class HTTP: GET: str = "GET"; POST: str = "POST"`. Under `slots=True`, the
class-level `GET` becomes a slot descriptor — `HTTP.GET` at runtime is
`<member 'GET' of 'HTTP' objects>`, not `"GET"`. Output:

```
<member 'GET' of 'HTTP' objects>
broken          # the `if HTTP.OK == 200:` test fell through
```

The associated `match` failure (`cases/116_match_class_attr.ty`) is
the same root cause: `case Status.GO:` compares the input value against
the slot descriptor and never matches.

This is the natural Typhon way to write "namespace of constants" (an
explicit `enum` is heavier and `ClassVar` requires `from typing import
ClassVar`). The path that "looks right" silently produces garbage.

Suggested fix surface (one of):

- Recognise `class X: NAME: T = literal` (no methods, no per-instance
  variability) and emit a non-dataclass module-level namespace.
- Lower defaulted class fields to `ClassVar[T] = literal` instead of
  dataclass fields, if the user clearly wants module-level constants.
- Emit `tyc::class_attr_shadows_slot` warning with a help text pointing
  users to `ClassVar` (`cases/122` confirms `ClassVar` works).

### R3.3 — Inline `with`-chain without `else err:` mis-parses

```python
def runner(x: int) -> Result[int, str]:
    with a = f(x)?, b = g(a)?:
        return Ok(b + 1)
```

Repro: `cases/119_with_chain_no_else.ty`.

Diagnostic: `tyc::invalid_question_op` at the first `?` in the binding,
saying "the `?` operator is only valid inside a function returning
`Result[T, E]`" — which is wrong (the function does return `Result`).

But the multi-line form with `else err:` (case 59) works fine.

Root cause appears to be that the `with`-chain syntax overlaps with
Python's regular `with` statement, and the parser only treats the line
as a Result chain when it sees the `else err:` clause. Without it, the
parser commits to the Python `with f(x)?:` reading and immediately
rejects `?` since the assignment isn't a stand-alone statement.

Suggested fix: when parsing `with`, look for `=` inside the head and
prefer the Result-chain reading if any binding contains `?`.

### R3.4 — Aliased `lazy import` blows up at runtime (severe)

```python
lazy import np = json

def main() -> None:
    let s: str = np.dumps([1, 2, 3])
    print(s)
```

Repro: `cases/31_lazy_import_alias.ty`.

Build succeeds. Run:
```
ValueError: module object for 'json' substituted in sys.modules during a lazy load
```

Un-aliased `lazy import json` (`cases/30`) works correctly.

Likely cause: the bespoke proxy class is registering itself under the
*alias* name `np` in `sys.modules`, but `json` is the real module name,
so `importlib`'s reverse lookup detects the substitution and refuses.

### R3.5 — `lazy let` inside `impl` block doesn't lower to `@cached_property`

```python
class Service:
    base: int

impl Service:
    lazy let derived: int = self.base * 10
    def get(self) -> int:
        return self.derived
```

Repro: `cases/29_lazy_let_class.ty`.

Output:
```
tyc::self_outside_impl

  × cannot find 'self' in scope
   ╭─[…/main.ty:7:24]
 6 │ class __typhon_impl_Service(object):
 7 │     let derived: int = self.base * 10
   ·                        ──┬─
   ·                          ╰── `self` is only valid inside an `impl` method body
```

The internal desugaring lowered `lazy let derived: int = ...` to a
class-body `let`, not a `@cached_property` method as the docs promise.
The `self.base` reference inside the body then fails resolve because
it's no longer in a method scope.

### R3.6 — Operator type-checking missing for `+` (and presumably others)

```python
def main() -> None:
    let n: int = 5
    let s: str = "hello"
    let r: str = s + n          # accepted at check, TypeError at runtime
```

Repro: `cases/130_string_concat.ty`, `cases/131_binop_type_mismatch.ty`.

`list + dict` likewise compiles. The diagnostic should fire at
`tyc check`; users running CI won't catch operator-type mistakes until
the test suite runs the line.

### R3.7 — Unknown kwarg to a constructor

```python
class User:
    id: int
    name: str

def main() -> None:
    let u: User = User(id=1, nmae="alice")
```

Repro: `cases/100_kwarg_wrong_name.ty`.

`tyc check` is silent. Runtime: `TypeError: User.__init__() got an
unexpected keyword argument 'nmae'`. Since constructor signatures are
statically known, the kwarg name typo should be flagged.

### R3.8 — Unknown field name in match pattern

```python
class Point:
    x: int
    y: int

def describe(p: Point) -> str:
    match p:
        case Point(z=z):
            return f"{z}"
        case _:
            return "?"
```

Repro: `cases/102_match_class_wrong_field.ty`.

Builds and runs. The match never fires (correctly — `z` isn't a field),
falls through to `_`. No diagnostic on the misspelled `z=`.

### R3.9 — `int / int` typed permissively in the LHS

```python
def main() -> None:
    let a: int = 10
    let b: int = 3
    let i: int = a / b
    print(i)
```

Repro: `cases/92_int_truediv_assigned_int.ty`.

Output: `3.3333333333333335`. The annotation `int` does not match the
truediv result `float`. Either:

- `/` should always yield `float`, and assigning `float` to `int` should
  fire `tyc::type_mismatch`, or
- there's a special `int / int -> float` inference rule that erodes when
  the RHS is annotated `int`.

Either way the current behaviour silently produces a float in an int
binding.

### R3.10 — Tuple index out of bounds at static type

```python
def main() -> None:
    let t: tuple[int, str] = (1, "hi")
    let z: bool = t[2]
```

Repro: `cases/134_index_out_of_typed.ty`.

Accepted at check time. Runtime: `IndexError`. The tuple arity is in
the type — a constant-index lookup should be checkable.

### R3.11 — Pipe into method confuses arg-count check

```python
class Counter:
    value: int

impl Counter:
    def add(self, n: int) -> int:
        return self.value + n

def main() -> None:
    let c: Counter = Counter(value=10)
    let v: int = c |> Counter.add(5)
```

Repro: `cases/08_pipe_method.ty`.

Diagnostic: `tyc::arg_count: expected 1, got 2`. The pipe lowering
yielded `Counter.add(c, 5)`, which is the right Python — but the impl
method signature is read as `(n: int) -> int` (without `self`), so the
checker sees a 1-arg target.

The runtime call works if the type-check is bypassed (Python tolerates
both `c.add(5)` and `Counter.add(c, 5)`). Two ways to fix:
- Pipe-lowering opts into the receiver-included calling convention so
  the arity check counts `self`.
- Impl-method type metadata records both `(self, n)` and `(n)` shapes.

### R3.12 — Match exhaustiveness gaps (five concrete shapes)

Each of these surfaces as `tyc::missing_return` even though the function
is actually total:

| # | Pattern | Repro |
|---|---|---|
| a | `case Point(x=x, y=y):` (kw-bind wildcard) | `cases/12_match_class_kw_only.ty`, `cases/26_match_exhaust_kw_wildcard.ty` |
| b | `case [*xs]:` (list star wildcard) | `cases/13_match_seq_star.ty`, `cases/27_match_seq_only_star.ty` |
| c | guarded variant then unguarded variant of same class | `cases/124_match_typed_int_dispatch.ty` |
| d | nested match inside an `Outer(inner=…)` arm | `cases/61_match_match_or.ty` |
| e | `int()` / `list()` patterns on a recursive type alias | `cases/140_type_alias_recursive.ty` |

A sixth shape that we'd expect to surface but is a *different* diagnostic:

| f | `case Point(x, y, z):` (wrong positional arity) | `cases/101_match_class_extra_field.ty` |

Here the surface diagnostic is still `tyc::missing_return`, but the
right one is "pattern `Point(x, y, z)` has 3 sub-patterns; class `Point`
has 2 fields".

### R3.13 — `?` in sub-expression position still rejected (open: #66)

```python
def go(s: str, t: str) -> Result[int, str]:
    return Ok(add(parse_int(s)?, parse_int(t)?))
```

Repro: `cases/02_question_in_argument.ty`. Diagnostic:
`tyc::invalid_question_op` with help-text "Rust-style mid-expression `?`
is not yet supported; lift the inner call to a `let` binding first".

The doc has carried this caveat for a while. Worth tracking until
addressed — `?` in argument position is the natural style.

### R3.14 — `async def` without `await` still no warning (open: #83)

```python
async def fake() -> int:
    return 42
```

Repro: `cases/104_async_no_await.ty`. Build succeeds with no diagnostic.

Docs promise a `tyc::async_without_await` warning when an `async def`
has no `await`.

### R3.15 — `tyc fmt` still near-no-op (open: #65)

```python
def    f(  x  :  int  ,   y :   int  ) ->    int :
    let a  :  int  =  1
    let b:int=2
```

After `tyc fmt`, the only change is whitespace inside the function head:

```python
def f(x : int, y : int) -> int :
    let a : int = 1
    let b:int=2          # no space normalisation
```

Untouched issues: `let b:int=2`, `id : int` annotation spacing, `f(1,2)`
comma spacing, `__name__=="__main__"` operator spacing.

Promise (docs): formatter is a Typhon-aware printer wrapped in `ruff
format`. Reality: it's near-identity.

### R3.16 — `@staticmethod` in `impl` mis-counts args

```python
class Counter:
    value: int

impl Counter:
    @staticmethod
    def make(n: int) -> "Counter":
        return Counter(value=n)

def main() -> None:
    let b: Counter = Counter.make(5)         # tyc::arg_count: expected 0, got 1
```

Repro: `cases/53_classmethod_static.ty`.

`Counter.zero()` (a `@classmethod`) works. `Counter.make(5)` reports
`expected 0, got 1` — the static signature is being read as if `n` were
stripped along with `self`.

### R3.17 — `stubs/` directory ignored (doc drift)

The guide shows `stubs/redis.dty` as the canonical location. In
practice, only `.dty` files inside the configured `src/` directory are
picked up by `tyc build`. Either the docs need to point at
`src/stubs/foo.dty` or the compiler needs a configurable stubs root.

### R3.18 — Emitted `.pyi` carries `@dataclasses.dataclass(slots=True)`

```python
# stubs.dty
class Counter:
    value: int

impl Counter:
    def inc(self) -> int
```

Emitted `build/mylib.pyi`:

```python
import dataclasses

@dataclasses.dataclass(slots=True)
class Counter:
    value: int

    def inc(self) -> int:
        ...
```

Most stub consumers (mypy, pyright, ty, IDEs) expect bare interface
declarations in `.pyi`. The dataclass decorator is an implementation
choice that doesn't belong in the surface API. Suggested fix: when
emitting `.pyi`, drop dataclass-implementation decorators and replace
defaulted fields with bare `: T` annotations.

---

## What's working

These cases compile and run correctly; many exercise features that have
been historically fragile:

- `cases/01_nested_result_chains.ty` — 3-deep Result + `?` chains.
- `cases/03_recursive_sealed.ty` — recursive sealed unions for trees.
- `cases/04_generic_result_unify.ty` — generic Result with type unification.
- `cases/05_nullable_chain.ty` — multi-level `guard` chains.
- `cases/06_pipe_chain_long.ty` — 5-stage `|>` pipe.
- `cases/07_pipe_into_user_fn.ty` — pipes through user functions.
- `cases/10_extend_str.ty` / `cases/34_extend_int.ty` — `extend builtin:`.
- `cases/16_async_gather_strict.ty` — strict `gather:` → TaskGroup.
- `cases/17_async_gather_best_effort.ty` — best-effort `gather` →
  `asyncio.gather(return_exceptions=True)`.
- `cases/18_async_go_no_handle.ty` / `cases/19_async_go_handle.ty` —
  `go f()` / `go f() -> task` both work, runtime registry holds refs.
- `cases/20-22_comptime_*.ty` — comptime literals, arithmetic, container
  literals, ternary, `comptime def` calls.
- `cases/23_pure_and_memo.ty` / `cases/24-25_pure_violation.ty` —
  `@pure` / `@memo` enforce all six conditions.
- `cases/30_lazy_import.ty` — un-aliased `lazy import`.
- `cases/38_interface_basic.ty` — interface conformance via `impl`
  block (was finding #26).
- `cases/39_interface_partial.ty` — missing methods rejected.
- `cases/41_class_frozen.ty` / `cases/42_class_frozen_assign.ty` —
  `frozen` modifier and frozen-field assignment rejection.
- `cases/46_walrus_let.ty` — walrus *parses* (the bug is in emission).
- `cases/49_comprehensions.ty` — list/dict/set/generator comprehensions.
- `cases/52_inheritance.ty` — `class Dog(Animal):` plus base-field
  access in `impl`.
- `cases/55_dunder_methods.ty` — `__add__`, `__str__` in `impl`.
- `cases/65_type_alias_generic.ty` — `type Vec[T] = list[T]`.
- `cases/85_decorator_factory.ty` — three-level decorator factory
  (was the gap behind #89).
- `cases/96_migrate_output.ty` — migrate now produces clean output that
  `tyc check` accepts.
- `cases/106_chained_method.ty` — `.inc().inc().double()` method chains.
- `cases/127_lazy_let_thread.ty` — module-level `lazy let` works
  end-to-end (was #84).
- `cases/129_dataclass_field_default.ty` — mutable defaults auto-wrapped
  in `dataclasses.field(default_factory=...)` (was #62).

---

## Suggested fix order

A pragmatic ranking by impact + ease:

1. **R3.1 walrus parens** — one printer fix. Critical correctness bug;
   silent miscompilation of every `while (x := …)` and `if (x := …) > …`.
2. **R3.2 class-constant footgun** — needs a design call (lower as
   `ClassVar` automatically vs. diagnostic) but the user surface is
   already badly broken. Pick a path, ship it.
3. **R3.3 inline with-chain parse** — parser disambiguation. Affects the
   "headline" idiomatic syntax the docs use.
4. **R3.6 operator types** — base `+`, `-`, `*` etc. on incompatible
   pairs should fire `tyc::type_mismatch`. Closes a major surface for
   silent runtime errors.
5. **R3.7 unknown kwarg in constructor** — should be the same code path
   that handles unknown kwargs in free function calls.
6. **R3.12 match exhaustiveness shapes** — the analyser needs to
   recognise keyword-binds-only-fields, star-only sequences, guarded +
   unguarded same-class pairs, and bind patterns (`int() as n`) as
   wildcards.
7. **R3.4 aliased lazy import** — runtime registry fix.
8. **R3.5 lazy let in impl** — desugar to `@cached_property`, as
   documented.
9. **R3.9 truediv typed-int LHS** — choose: float-only result or
   narrow-on-LHS coercion check.
10. **R3.11 pipe-into-method arity** — minor papercut.
11. **R3.16 @staticmethod arity** — minor papercut; less common in code
    than R3.11.
12. **R3.10 tuple-out-of-arity index** — type system gap.
13. **R3.13 / R3.14 / R3.15** — known-open from prior rounds; track.
14. **R3.17 / R3.18** — docs / pyi cleanup.

Cumulatively the type-checker gaps (R3.6, R3.7, R3.8, R3.9, R3.10, R3.16)
form a pattern: simple statically-checkable mistakes (wrong operand
type, wrong kwarg, wrong field, wrong tuple index) reach runtime as
opaque `TypeError`s and `IndexError`s. Closing this batch would
dramatically tighten the "stricter superset" promise.
