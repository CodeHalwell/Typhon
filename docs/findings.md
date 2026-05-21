# Typhon — Findings

This is the single source of truth for stress-test findings across every
campaign that has run against `tyc` so far. It supersedes (and replaces):

- the old top-level `FINDINGS.md` (campaigns through 2026-05-20),
- `EXAMPLES_REVIEW.md` (the 47-example sweep that produced findings F1–F5),
- `docs/follow-ups-2026-05-17.md` (the May 2026 completion sprint),
- `stress/round-2026-05-20/FINDINGS.md` (R3.1–R3.18),
- `stress/round-2026-05-20-exploration/FINDINGS.md` (E1–E11),
- `stress/round-2026-05-21/FINDINGS.md` (B1–B14).

The original `stress/round-*/` test corpora are kept on disk under
`stress/` so each finding's repro file can still be replayed; only the
write-ups have been consolidated here.

---

## Quick status

**Eight stress-test campaigns** have been run between May 17 and May 21,
2026. Across them ~600 hand-written `.ty` programs were authored
spanning language edge cases, IO, ML/numpy, mock LLM/RAG, agents, APIs,
SDK patterns, perf, and intentionally-broken diagnostic probes. Roughly
**120 distinct findings** were filed.

| Round | Branch | New findings | Verdict |
|---|---|---|---|
| 2026-05-17 sprint | `claude/assess-project-completion-gUdOP` | sprint follow-ups (see [Sprint history](#sprint-history)) | mostly closed |
| 2026-05-18 #1 | `claude/test-typhon-library-EnZmE` | #1–#36 | all closed except #18 |
| 2026-05-18 fix-up sweep | `claude/update-findings-IdfrH`, `claude/fix-findings-diagnostics-hYj8K` | — | closed 33 of 36 |
| 2026-05-18 #2 | `claude/test-typhon-library-kxGRX` | #37–#56 | all closed in `resolve-open-findings-d6EIV` |
| 2026-05-18 examples sweep | `claude/review-examples-x5kgr` | F1–F5 (cross-cutting), 7 compiler bugs | compiler bugs closed; F1–F4 still open (see below) |
| 2026-05-19 | `claude/test-typhon-library-rNIYC` | #57–#127 | all but the deferred items closed in `review-findings-fixes-VRFJy` / `resolve-open-findings-UDZfv` |
| 2026-05-20 | `claude/test-typhon-library-ejNr5` | R3.1–R3.18 | closed in `resolve-open-findings-v6t65` except `tyc fmt` and `?` in sub-expr |
| 2026-05-20 exploration | `claude/typhon-exploration-testing-LZezp` | E1–E11 | **most still open** (see below) |
| 2026-05-21 | `claude/tender-hawking-LLhuR` | B1–B14 | **all open** (this is the freshest round) |

**Pass rate trend** on the canonical example suite (`examples/01-…46-…`):
20/47 → 39/47 → 46/46 → 46/46. The examples now build and run end-to-end
on every commit; new findings come from the stress corpora rather than
the curated examples.

---

## Open findings

Severity legend: **CRITICAL** (silent wrong output / runtime crash on
documented happy path), **HIGH** (documented feature unusable),
**MEDIUM** (feature works with workaround), **LOW** (UX / DX nit /
docs).

### CRITICAL — silent miscompile / wrong runtime behaviour

#### O1 — Emitter strips parens around binary expressions in some suffix contexts *(E1)*

Repro: `stress/round-2026-05-20-exploration/08-meta-stress/25-paren-emit-suite.ty`,
`08-meta-stress/03-paren-wrong-output.ty`.

```python
# Typhon source:
let s: str = ("a" + "b").upper()
# Emitted Python:
s: str = "a" + "b".upper()         # == "aB", not "AB"

# Typhon source:
let b: bool = (True or False) and False
# Emitted Python:
b: bool = True or False and False  # == True, not False
```

Real-world impact also seen on `(root / "a.txt").write_text(...)`
collapsing to `root / "a.txt".write_text(...)` and on numpy
least-squares fits collapsing to `dx * dy.sum() / dx * dx.sum()`.

The `tyc-emit` `needs_paren` table is missing the case where a
parenthesised binary expression is followed by attribute access, method
call, subscript, or a lower-precedence boolean op. Parens around the
*inner* operand of a higher-precedence arithmetic op (`(3 + 4) * 2`) are
preserved — only the suffix-context shapes regress.

### HIGH — documented features unusable

#### O2 — Flow narrowing fades inside `while x is not None:` loop bodies *(B5)*

Repro: `stress/round-2026-05-21/08-meta-stress/21-narrowing-loop.ty`,
`22-narrowing-while-only.ty`, `20-self-type.ty`.

```python
def sum_list(head: Node?) -> int:
    mut total: int = 0
    mut cur: Node? = head
    while cur is not None:
        total = total + cur.value     # tyc::nullable_use on cur.value
        cur = cur.next                # tyc::nullable_use on cur.next
    return total
```

`cur` is narrowed at the loop check but the narrowing drops the moment
*any* body statement mutates anything. Even rebinding the value with
`let n: Node = cur` immediately inside the loop fails — narrowing
doesn't reach the assignment site. The recursive form (parameter that
never rebinds) works fine; the loop form, which is the iterator idiom,
does not.

#### O3 — `tuple[T, ...]` variadic tuple type rejects tuple literals *(B1)*

Repro: `stress/round-2026-05-21/08-meta-stress/11-tuple-variadic-bug.ty`.

```python
let xs: tuple[float, ...] = (1.0, 2.0, 3.0)   # tyc::type_mismatch
let ys: tuple[float, ...] = ()                # tyc::type_mismatch
```

The diagnostic prints the expected type as `tuple[float, ?]` — the
variadic-marker `...` is rendered as a placeholder, suggesting the
unifier never recognises `tuple[T, ...]` as homogeneous. The cheatsheet
documents this spelling; today users must downgrade to `list[float]`,
losing hashability and positional indexing.

#### O4 — Recursive `type` alias rejected as a cycle *(B2)*

Repro: `stress/round-2026-05-21/08-meta-stress/12-recursive-type-alias-bug.ty`.

```python
type JSON = None | bool | int | float | str | list[JSON] | dict[str, JSON]
# tyc::alias_cycle + tyc::type_mismatch on every use
```

A self-referencing alias through `list[…]` / `dict[str, …]` is the
canonical shape for JSON, ASTs, trees, anything self-similar. Workaround
today is `dict[str, object]`, which discards typing.

Note: #57/#58/#70 made *non-recursive* aliases transparent for
assignability. Recursive cycles still terminate cycle detection too
eagerly. The cycle terminator at depth 8 added in the May 19 sprint does
not help here because the rejection is at the resolve stage, not the
assignability stage.

#### O5 — `class!` Exception subclass synthesises arg-less `__init__` *(B4)*

Repro: `stress/round-2026-05-21/08-meta-stress/14-exception-class-bug.ty`.

```python
class! AppError(Exception):
    pass

raise AppError("hello")        # TypeError: __init__ takes 1 positional, got 2
```

The synthesised init drops the message arg. For exceptions in
particular — and any raw-class with a non-trivial parent more broadly —
the emit should generate
`def __init__(self, *args, **kwargs): super().__init__(*args, **kwargs)`
when the body has no fields. Today's workaround forces every exception
to declare a `message: str` field and use kwarg-only raises
(`raise AppError(message="…")`), losing the conventional positional
form every Python programmer expects.

#### O6 — `return` inside a generator function flagged as value-return mismatch *(B3)*

Repro: `stress/round-2026-05-21/08-meta-stress/13-generator-return-bug.ty`,
`07-sdk/02-paginator.ty`.

```python
def stop_early(n: int) -> Iterator[int]:
    for i in range(n):
        if i > 5:
            return        # tyc::type_mismatch: expected Iterator[int], found None
        yield i
```

`return` inside a generator is `StopIteration`, not a value return. The
checker reads the surrounding function as a regular
`def Iterator[int]:` and demands the return statement produce an
`Iterator[int]`. The `tyc::generator_return_type` warning added in
`resolve-open-findings-d6EIV` catches the *opposite* direction (yield in
non-iterator return); the same machinery needs to flip the
return-statement validator inside a generator body.

#### O7 — `impl X:` methods reject `T?` parameters *(E2)*

Repro: `stress/round-2026-05-20-exploration/08-meta-stress/06-nullable-impl-method.ty`,
`07-sdk-client/04-pagination-gen.ty`.

```python
class API:
    name: str

impl API:
    def fetch(self, cursor: str?) -> int:
        return 0 if cursor is None else len(cursor)

def main() -> None:
    let api: API = API(name="x")
    let v: str? = None
    print(api.fetch(v))    # tyc::nullable_use — "value is `? | None`"
    print(api.fetch(None)) # tyc::type_mismatch  — "expected `?`, found `None`"
```

The diagnostic misrenders the parameter type as a bare `?` instead of
`str?`, suggesting the resolver attaches `Nullable<unknown>` to
impl-method params. A free function with the same signature works
correctly. Every typed SDK / repository / service with an optional
argument hits this.

#### O8 — `return self` from `impl __enter__` fails type-check *(E3)*

Repro: `stress/round-2026-05-20-exploration/08-meta-stress/23-context-mgr-impl.ty`,
`02-io-heavy/09-context-manager-custom.ty`.

```python
class Stopwatch:
    label: str
    start: float

impl Stopwatch:
    def __enter__(self) -> Stopwatch:
        self.start = time.monotonic()
        return self          # tyc::type_mismatch: expected Stopwatch, found __typhon_impl_Stopwatch
    def __exit__(self, ...) -> None: ...
```

The synthetic `__typhon_impl_Stopwatch` pseudo-class is leaking out of
the desugarer into the type-check error surface. Blocks the most common
context-manager shape in Python (timers, spans, locks, transactions).

#### O9 — `type Handler = Callable[[Req], Resp]` doesn't unwrap on call *(E4)*

Repro: `stress/round-2026-05-20-exploration/06-api-server/03-middleware.ty`.

```python
type Handler = Callable[[Request], Response]

def with_auth(next: Handler) -> Handler:
    def wrapped(req: Request) -> Response:
        return next(req)   # tyc::type_mismatch: expected Response, found Handler
    return wrapped
```

`Callable` aliases aren't transparent when the value is called. Inlining
the `Callable[[Request], Response]` works but defeats the alias.
Middleware-style typed pipelines and FP-shaped layered designs are
painful.

### MEDIUM — workable but rough

#### O10 — Pattern-binding names collide with outer `let` bindings *(B10)*

Repro: `stress/round-2026-05-21/08-meta-stress/15-pattern-name-scope.ty`,
`05-agents/01-react-agent.ty` (initial form).

```python
let value: int = 99
match b:
    case Wrap(value):              # tyc::immutable_assign
        print(value)
```

Pattern variables in `case Foo(name):` are conceptually new bindings,
not re-assignments. Treating them as the same name as an enclosing
`let` is consistent with Rule-2 / function scope but every
Rust/OCaml/Scala programmer expects pattern bindings to introduce fresh
names. Either (a) introduce a per-`case` scope, or (b) upgrade the
diagnostic to `tyc::pattern_shadows_outer` with a clear rename hint.

#### O11 — `for x in ...` cannot rebind an outer `let x` in the same scope *(F1, E10)*

Hit in `36-huggingface-transformer`, `40-llm-tool-use`,
`43-agent-framework`, and `47-mini-app/agent.ty`. Pattern:

```python
mut text_parts: list[str] = []
for block in resp.content:        # introduces `block` as a let
    ...
mut tool_results: list[dict[str, object]] = []
for block in resp.content:        # tyc::immutable_assign — can't rebind
    ...
```

Python users will write this pattern reflexively. The docs list walrus
and `gather:` as exceptions to Rule 2 but not `for` targets. Either (a)
scope the for-loop binding to the loop body, or (b) accept the rebind
silently when the body completes, or at minimum (c) reword the
diagnostic to match the `for`-target intuition.

#### O12 — `tyc fmt` is a near-no-op *(B9, R3.15, #18, #65, #122)*

Repro: feed any `.ty` with cramped spacing through `tyc fmt`. Tracked
across every campaign since the very first.

```python
def    f(  x:int,y:int)->int:
    let    z:int=x+y
```

After `tyc fmt`:

```python
def f(x:int,y:int)->int:    # missing spaces around : , -> = preserved
    let z:int=x+y
```

The Phase-5 Typhon-aware printer documented at
`tyc-format/src/lib.rs:17` remains future work. Ruff is on PATH but
ruff doesn't speak `let`/`mut`, so the fmt wrapper can only do a
partial whitespace pass.

`tyc build` *does* run `ruff format` over the emitted Python (correctly,
end-to-end). The deliverable is clean; the source isn't.

#### O13 — Pydantic dep not auto-injected when project lacks `[dependencies]` *(B14)*

The fix from `#103` adds `pydantic = "*"` to a project's synthesised
`pyproject.toml` when any source file contains `model X:`. However when
a project's `typhon.toml` has no `[dependencies]` section at all, the
auto-inject can fail to surface (depends on the bootstrapping path
taken). Reproduced in the round-2026-05-21 stress harness, which
synthesises a fresh `typhon.toml` per case. Either tighten the detection
path or document the requirement to declare at least an empty
`[dependencies]` table for the auto-inject to fire.

#### O14 — `unsafe:` value leak isn't caught *(#107)*

A binding declared inside `unsafe:` can be returned from a function
whose annotated return type is concrete (e.g. `-> int`) without
re-asserting, contradicting Rule 5 in the language spec. Needs a real
`Unsafe[T]` marker type in `tyc-types` and a flow-sensitive pass through
the block boundary.

#### O15 — TypedDict dict-literal inference *(#108)*

```python
let alice: User = {"id": 1, "name": "Alice"}
# tyc::type_mismatch: expected User, found dict[str, int | str]
```

The `User(id=…, name=…)` constructor form works; the dict-literal form
doesn't. Needs the type-checker to register TypedDict-derived class
shapes and match dict literals against the expected field set.

#### O16 — `Protocol`-based structural conformance against built-ins *(#112)*

```python
class Sized(Protocol):
    def __len__(self) -> int: ...

def take(s: Sized) -> int: return len(s)

take([1])      # tyc::type_mismatch: expected Sized, found list[int]
```

Built-ins don't participate in structural conformance against
user-declared `Protocol`s. Needs the type-checker to know which
built-in types implement which dunder methods (`__len__`, `__iter__`,
etc.).

#### O17 — `?` operator only works at the top of an assignment / statement *(#66, R3.13, E9)*

```python
def f(s: str, t: str) -> Result[int, str]:
    return Ok(add(parse(s)?, parse(t)?))   # tyc::invalid_question_op
```

The diagnostic is now crisp ("lift the inner call to a `let` binding
first") and points at the user's source — that landed in the May 19
sprint. Lifting the limitation itself (rewriting `Ok(f(x)?)` into a
temp + propagation) remains future work and is the natural Rust-style
form users reach for.

#### O18 — `__mul__(self, scalar: float)` ignored when right operand is a float literal *(E5)*

Repro: `stress/round-2026-05-20-exploration/08-meta-stress/22-dunder-ops.ty`.

`impl V: def __mul__(self, scalar: float) -> V` is rejected for
`let d: V = a * 5.0`. The same code with
`__mul__(self, scalar: int)` and `a * 4` works. The checker short-
circuits to the builtin `float.__mul__` instead of consulting the
user-defined overload.

#### O19 — Exhaustiveness false negatives on sealed unions with positional captures *(E6, partial)*

Several legitimate-total matches still mis-fire as
`tyc::missing_return`. R3.12 closed five of the most common shapes;
recursive sealed types with positional captures, multi-variant
dataclass enums with empty variants, and three-variant tool unions
with positional captures still surface as false positives. Repros at
`01-syntax-edge/05-sealed-recursive.ty`,
`05-agents/03-agent-state-machine.ty`,
`04-ai-llm/03-llm-tools-sealed.ty`.

Workaround `case _:` works but defeats the safety property.

#### O20 — Comptime f-strings rejected *(E7)*

```python
comptime let APP: str = "myapp"
comptime let MAJOR: int = 1
comptime let MINOR: int = 0
comptime let VERSION: str = f"{APP} v{MAJOR}.{MINOR}"   # rejected
```

Workaround: `str(MAJOR) + "." + str(MINOR)` works. F-strings are the
first thing anyone reaches for when building a comptime version
string.

#### O21 — `tyc migrate` produces unparseable `"Item"? = None` *(B6)*

Feed any `.py` with `parent: Optional["Item"] = None` through
`tyc migrate`. Emitted: `parent: "Item"? = None`. The correct rewrite
is `parent: "Item?" = None`. The migrated `.ty` doesn't even build.

#### O22 — `tyc migrate` doesn't rewrite `Union[T, None]` → `T?` *(B7)*

Same input class. `Optional[T]` is rewritten but `Union[T, None]` is
not, and `from typing import Union` is left dangling. Documented as
"conservative" — but `Union[T, None]` is identical to `Optional[T]`
and hits real-world code at the same rate.

### LOW — DX / cosmetic

#### O23 — `extend list[int]:` (parametric) mis-targets `list` *(B8)*

Repro: `stress/round-2026-05-21/01-language-edge/16-extend-builtin.ty`
(initial form).

```python
extend list[int]:
    def total(self) -> int: ...
```

Diagnostic claims the user wrote `impl list:`. Either accept
parametric extends or surface a
`extend on parameterised types is not supported yet` error.

#### O24 — VM repr disagrees with CPython repr for `Result` *(B11)*

VM prints `Ok(20)`; CPython prints `Ok(value=20)`. Causes `tyc run`
and `tyc run --compile` to diverge in stdout, which the `tyc-vm` doc
explicitly warns about, but worth surfacing for screenshot-driven
docs and test fixtures.

#### O25 — `tyc explain --list` advertised but unimplemented *(B12)*

When `tyc explain not_a_real_code` fails, the help text reads:

> Run `tyc explain --list` (not yet implemented) or see https://typhon.dev/lang/diagnostics

"Not yet implemented" in a user-visible message is its own problem.
Either land the subcommand or rewrite the suggestion.

#### O26 — REPL prompt prints stacked `>>>` on empty / pasted multi-line input *(B13)*

```
>>> >>> >>> 6
```

The block-end-on-blank-line behaviour is documented but the prompt
state makes a paste look broken.

#### O27 — Sequence pattern `[a, b]` emits as tuple pattern `(a, b)` *(#111)*

PEP 634 makes these semantically identical (both match any sequence)
and the runtime behaviour is correct. Round-tripping the original
bracket choice requires source-range tracking on the pattern node
and is purely cosmetic.

#### O28 — Pipe corner cases *(#117, #118, #119)*

- `5 |> (lambda x: x * 2)()` is a parse error — pipe RHS doesn't
  accept a parenthesised lambda call.
- `s |> S.add(5)` desugars to `S.add(s, 5)` and the call-site arity
  check rejects it because `impl`-block method types don't include
  `self` in the function arity the checker sees. (R3.11 closed the
  basic pipe-into-impl case; the variant where `self` is the
  receiver via class-name dispatch is still open.)
- `(1 |> add(2)) |> add(3)` — pipe `|>` is not recognised inside a
  parenthesised sub-expression: parser fails at the inner `>`. Pipe
  is restricted to top-of-statement positions.

Pipe expansion is a top-of-statement-line pre-pass; lifting it into
general expression position needs a lexer-aware rewrite.

#### O29 — Per-invocation `uv sync` provisioning *(E11)*

Every `tyc build` invocation against a project with non-empty
`[dependencies]` spawns `uv sync` and may reprovision `.venv` from
scratch. For one-off `.ty` files under tmp directories (stress
harnesses, REPL-like testing), this dominates wall-clock time.
A `--no-deps` / `--reuse-venv` flag would speed iteration.

---

## Recommended next steps

Roughly ranked by impact per unit of work:

1. **O1 (paren-stripping)** — one printer fix, silent correctness bug.
   Lands in real ML and pathlib code.
2. **O5 (`class!` Exception `__init__`)** — one synthesised
   constructor; unblocks the conventional `raise Foo("msg")` form.
3. **O2 (loop-body narrowing)** — extending narrowing to "survives
   until the next assignment to the narrowed name" unlocks the
   linked-list iterator pattern.
4. **O7 (impl-method `T?` params)** — every typed SDK with an
   optional arg trips this.
5. **O8 (`return self` from `__enter__`)** — most common
   context-manager shape; one synthetic-class leak to plug in the
   type-check error surface.
6. **O3 (`tuple[T, ...]`)** — small unifier fix, very visible doc
   correctness win.
7. **O9 (`Callable` alias)** — unblocks middleware / decorator
   patterns once and for all.
8. **O6 (generator `return`)** — flip one branch in the
   return-statement validator when the function has any `yield`.
9. **O4 (recursive type alias)** — at minimum surface a clear
   `not yet supported` diagnostic instead of the cascading mismatch
   firehose.
10. **O10 + O11 (pattern shadowing, for-target rebind)** —
    closely-related DX wins; pick one or the other to overhaul the
    rule.
11. **O12 (`tyc fmt`)** — pick three rules
    (no-space-before-colon, single-space-around-`=`/`+`, two-blank-
    lines before top-level defs). Anything is better than today.
12. **O18 (`__mul__` resolution)** — operator-overload resolution
    order needs to consult user impls before builtin scalar rules.

The remaining open items are smaller papercuts.

---

## Sprint history

For full historical detail of how each closed finding landed, see the
git history of the per-branch commits referenced below. This section
records *what* shipped in each branch; the individual finding write-ups
have been retired now that they're closed.

### May 17 — completion sprint (`assess-project-completion-gUdOP`)

Landed: module-level + class-level `lazy let` lowering; `extend BUILTIN:`
implementation; `unsafe:` block depth tracking; PEP 695 typevar
substitution; `tyc trace`; `tyc profile`; `tyc migrate`; LSP hover /
go-to-definition; `tyc check --stubs`. 514 unit tests at sprint end (up
from 371).

Deliberately deferred → since landed: ruff-parser fork (vendored),
auto-gather inference + parallelisation, source-map line accuracy,
`class!` raw-class modifier with synthesised `__init__`, runtime
`stubtest` probe, comptime `def` functions, variance in `assignable`.

Deliberately deferred → still future work: `ty` integration Phase 2
(embedded library sharing the Salsa DB); higher-kinded /
bounded-typevar inference; vendoring `ruff_python_codegen` (probed and
rejected as net-negative).

### May 18 round 1 (`test-typhon-library-EnZmE` → fixed across `update-findings-IdfrH`, `fix-findings-diagnostics-hYj8K`)

Filed #1–#36, all closed except #18 (`tyc fmt`, still open as O12).
Major closures: `class Foo frozen:`, `guard NAME = EXPR else: …` (single
and multi-line), `impl[T] Box[T]:`, `gather(strategy="best-effort")`
scope, `T?` Union/Union assignability, `dict.get(k)` as `V?`,
`if x:` truthy narrowing, lazy import diagnostics, interface
conformance via `impl`-block methods, `tyc check` parity with build for
purity/comptime, missing param/return annotation enforcement, float
literal `1.0` emission, `from __future__ import annotations` injection,
comptime container literals + string methods, `tyc::result_error_mismatch`,
`tyc::method_in_class_body` with strictness toggle,
`tyc::auto_gather_missed` advice, REPL auto-print.

### May 18 round 2 (`test-typhon-library-kxGRX` → fixed in `resolve-open-findings-d6EIV`)

Filed #37–#56, all closed. Major closures: `with`-chain / best-effort
`gather:` / `go` desugarers emit `let` (Rule-2 regression cluster);
`for k, v in d.items():` tuple-target declaration; f-string format
specs / `!r`/`!s`/`!a` conversions; f-string nested same-quoted strings;
`Callable[[P], R]` callability; kwargs / defaults / `*args` arity
counting; generic constructor type-param inference (bidirectional +
forward); PEP 695 lowering for older targets; comptime cross-binding
scope + `comptime def` dispatch; `tyc::missing_await`; `tyc::manual_init`;
`tyc::generator_return_type` (warn direction); multi-line pipe; REPL
None-call skip; `extend BUILTIN:` self annotation; `tyc init NAME`
scaffolds into `./NAME/`; examples-suite pass rate 20/47 → 39/47.

### May 18 examples sweep (`review-examples-x5kgr`)

Closed 7 compiler bugs surfaced by walking every `examples/` project:
resolver `Stmt::Match` walk; resolver parameter-default walk; resolver
nested function / class declaration; `with`-chain `?` validator false
positive; format `apply_simple_style_rules` whitespace stripping;
f-string literal brace re-escaping; comptime `def` emission. Examples
suite ended at 39/47 build + 39/47 stdlib-runnable.

Filed F1–F5 cross-cutting findings: F1 (loop-target rebind, **still
open as O11**), F2 / F3 / F4 (missing-return analysis through `unsafe:`,
`with`, and exhaustive `match` — the May 19 / May 20 sprints chained
the missing-return checker into the relevant analyses), F5 (docs nit
about declaring deps to make `tyc check` happy in a fresh
`tyc init` project — added to `examples/README.md`).

### May 19 round (`test-typhon-library-rNIYC` → fixed in `review-findings-fixes-VRFJy`, `add-library-autocomplete-1N2iS`, `resolve-open-findings-UDZfv`)

Filed #57–#127. Closures span: transparent type aliases (#57/#58/#70),
method-level typevars on `impl[T]` (#59), dependent `gather:` bindings
fall back to sequential (#60), `global` / `nonlocal` + Rule 2 (#61),
mutable dataclass defaults (#62), `@property` typing (#63),
`tyc migrate` correctness (#64), partial `tyc fmt` (#65 — still mostly
open as O12), `?` diagnostic crisp (#66 — limitation **still open as
O17**), bare `list` / `dict` / `tuple` annotations rejected (#72),
`from typing import TypeVar` rejected (#73), `typing.List` / `Dict` /
... rewritten (#74), for-loop target binding kind (#75), block-level
shadowing diagnostic (#76), class redeclaration (#77),
`impl UnknownClass:` (#78), unknown module at check time (#79),
unknown-kwarg "did you mean" (#80), circular alias (#81), missing-return
exhaustive-match chain (#82), `async def` no `await` warning (#83),
`lazy let` runtime emission (#84), auto-gather decorator gate (#85),
`*args: T, sep: str = "-"` (#86), comptime help text (#87), comptime
subscript (#88), decorator-factory docs path (#89), `self` outside
`impl` (#90), `let x: T` no init (#91), `main()` not called advice
(#92), inline comment in `gather:` (#93). Typing-bridge transparency
across `Self` / `Literal` / `Final` / `ClassVar` / `Annotated`
(#97–#100). `class X(NamedTuple):` and multi-inheritance no longer
emit `slots=True` (#101 / #102). `model X:` auto-injects pydantic
(#103). `type Err = …` no longer shadows `?` runtime constructor (#104).
`lazy let` print materialises (#105). `f"{x=}"` debug repr (#106).
`ExceptionGroup` / `BaseExceptionGroup` in builtin scope (#113).
async-protocol dunder carve-out (#114). `tyc migrate` typing-alias
rewriter (#115). `bytes` literal preservation (#116). `tyc init` entry
block (#123). Walrus carve-out documented (#124). `tyc fmt` still open
as O12. Doc drift on `lazy_val` → `lazy_let` (#126 / #127).

Still open after this round:
- #107 — `unsafe:` value leak → **O14**.
- #108 — TypedDict dict-literal inference → **O15**.
- #111 — sequence pattern bracket round-trip → **O27**.
- #112 — Protocol vs built-ins → **O16**.
- #117 / #118 / #119 — pipe corner cases → **O28**.
- #122 — `tyc fmt` → **O12**.

### May 20 round (`test-typhon-library-ejNr5` → fixed in `resolve-open-findings-v6t65`)

Filed R3.1–R3.18, all closed except `tyc fmt` (R3.15) and `?` in
sub-expression (R3.13). Major closures: walrus parens preserved (R3.1);
class-attr-shadows-slot warning (R3.2); inline `with`-chain (R3.3);
aliased `lazy import` runtime registry (R3.4); `lazy let` inside `impl`
→ `@cached_property` (R3.5); `tyc::operator_type_mismatch` for
clearly-wrong operand pairs (R3.6); kwarg validation in constructors
(R3.7); match-class pattern field validation (R3.8); `int / int` typed
as `float` (R3.9); `tyc::tuple_index_out_of_range` (R3.10); pipe-into-
method arity counts `self` (R3.11); five match exhaustiveness shapes
(R3.12); `async_without_await` (R3.14 / #83 redux); `@staticmethod` /
`@classmethod` in `impl` arity (R3.16); `stubs/` directory doc fix
(R3.17); `.pyi` stub decorator stripping (R3.18).

### May 20 exploration (`typhon-exploration-testing-LZezp`)

Filed E1–E11. Verified-fixed: R3.1, R3.6, R3.7, R3.8, R3.9, R3.10,
R3.11, R3.14, R3.16. Still open: E1 → **O1**, E2 → **O7**, E3 → **O8**,
E4 → **O9**, E5 → **O18**, E6 → **O19**, E7 → **O20**, E9 → **O17**,
E10 → **O11**, E11 → **O29**. (E8 was a re-statement of R3.2, closed
in `resolve-open-findings-v6t65`.)

### May 21 round (`tender-hawking-LLhuR`) — this branch

Filed B1–B14 (81 fresh `.ty` programs, 65/81 build + run clean; 7 real
bugs surfaced). Every B-finding maps onto an entry in the Open list
above:

- B1 → **O3** (variadic tuple)
- B2 → **O4** (recursive alias)
- B3 → **O6** (generator return)
- B4 → **O5** (`class!` exception init)
- B5 → **O2** (loop narrowing)
- B6 → **O21** (migrate forward-ref)
- B7 → **O22** (migrate `Union[T, None]`)
- B8 → **O23** (parametric `extend`)
- B9 → **O12** (`tyc fmt`)
- B10 → **O10** (pattern shadowing)
- B11 → **O24** (VM repr)
- B12 → **O25** (`tyc explain --list`)
- B13 → **O26** (REPL prompt)
- B14 → **O13** (Pydantic dep auto-inject)

Repro corpus + run script at `stress/round-2026-05-21/`. The
intentionally-broken probes under `10-error-quality/` exist to validate
diagnostic message quality and are expected to fail at build time.
