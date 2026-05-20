# Typhon — Exploration & Stress-Test Findings

Date: 2026-05-20
Branch: `claude/typhon-exploration-testing-LZezp`
Compiler: `tyc 0.2.0`, built from this commit (`cargo build --release`).
Python: `/usr/bin/python3.13` (3.13.12).

## Scope

~50 fresh `.ty` programs across eight domains, exercising features that
the prior 2026-05-19 / 2026-05-20 rounds touched less deeply: real ML
math chains, multi-agent loops, full SDK shapes, validation pipelines,
LLM tool dispatch, context-manager protocols, operator overloading,
PEP 695 generators, and the corners of the type system around `T?` /
sealed unions / impl methods.

Each case lives under `stress/round-2026-05-20-exploration/`:

```
01-syntax-edge/   (20 cases)  — language surface
02-io-heavy/      (10 cases)  — files, csv, regex, sqlite, threading, async-io
03-ml-numpy/      ( 5 cases)  — numpy ops, pandas, pure stats, k-NN
04-ai-llm/        ( 5 cases)  — mock LLM client, streaming, tool dispatch, RAG, structured-out
05-agents/        ( 3 cases)  — ReAct agent, multi-agent gather, state machine
06-api-server/    ( 4 cases)  — router, CRUD, middleware, validation chain
07-sdk-client/    ( 5 cases)  — typed SDK, retry, rate limit, paginator, event bus
08-meta-stress/   (~16 cases) — targeted probes for individual bugs
```

`build_and_run.sh` runs each through `tyc build` and (when build passes)
the emitted `python3.13 build/main.py`. `SHOW_EMIT=1` dumps the
generated Python in between.

## Score

- 39 cases built and ran clean (happy paths covering everything from
  file I/O and sqlite through async TaskGroups, RAG cosine-similarity
  retrieval, pydantic validation, and recursive sealed-union sums).
- ~10 cases hit fresh failures or regressions documented below.
- 5 of the previously-reported open R3 findings are confirmed **fixed**
  (kwargs, pattern fields, truediv, decorator-in-impl, async-no-await,
  operator type-check, tuple arity, pipe-into-impl); 4 are confirmed
  **still open**.

## Verified-fixed since round 2026-05-20

| Prior | What | Verifying case |
|---|---|---|
| **R3.1** | Walrus parentheses preserved at emit | `01-syntax-edge/01-walrus-nested.ty` |
| **R3.6** | `str + int` rejected with `tyc::operator_type_mismatch` | `08-meta-stress/15-operator-strict.ty` |
| **R3.7** | Unknown kwarg in constructor caught | `08-meta-stress/08-bad-kwargs.ty` |
| **R3.8** | `case Point(z=z)` rejected (Point has no `z`) | `08-meta-stress/12-pattern-bad-field.ty` |
| **R3.9** | `let bad: int = a / b` flagged (truediv → float) | `08-meta-stress/10-truediv-types.ty` |
| **R3.10** | `t[2]` for `tuple[int, str]` flagged out-of-range | `08-meta-stress/16-tuple-arity-strict.ty` |
| **R3.11** | `c \|> Counter.add(5)` checks correctly | `08-meta-stress/24-pipe-into-impl.ty` |
| **R3.14** | `async def` with no `await` warns | `08-meta-stress/19-async-warn.ty` |
| **R3.16** | `@classmethod` / `@staticmethod` in impl call correctly | `08-meta-stress/14-decorators-impl.ty` |

## New findings (this round)

### E1 — Silent paren-stripping in emit causes wrong runtime behaviour  *(CRITICAL)*

When the emitter prints a parenthesised binary expression whose **outer**
context is attribute access, method call, subscript, or a *lower*-
precedence boolean op, it drops the parens. Output is syntactically valid
Python but parses differently.

Repros (`08-meta-stress/25-paren-emit-suite.ty`, `08-meta-stress/03-paren-wrong-output.ty`):

```python
# Typhon source:
let s: str = ("a" + "b").upper()
# Emitted Python:
s: str = "a" + "b".upper()         # == "aB", not "AB"

let b: bool = (True or False) and False
# Emitted Python:
b: bool = True or False and False  # == True, not False
```

Real-world impact, surfaced in `02-io-heavy/03-pathlib-walk.ty` (a
naive `pathlib.Path / "x"` write):

```python
(root / "a.txt").write_text("hello")
# emits:
root / "a.txt".write_text("hello")
# → AttributeError: 'str' object has no attribute 'write_text'
```

…and in `03-ml-numpy/02-numpy-vectorops.ty` (least-squares fit on numpy):

```python
let slope: float = float((dx * dy).sum() / (dx * dx).sum())
# emits:
slope: float = float(dx * dy.sum() / dx * dx.sum())
# → numpy returns wrong values; runtime TypeError when float() of an array
```

Inconsistency: parens around the *inner* operand of a higher-precedence
arithmetic op **are** preserved (`(3 + 4) * 2` emits correctly with
parens, yields 14). The bug is specifically in the cases where the outer
operator's precedence sits between "attribute access / call" and
arithmetic. Treat this as a hole in the emitter's `needs_paren` logic.

Affected forms (confirmed):
- `(BIN_OP).attr_access`
- `(BIN_OP).method_call(...)`
- `(BIN_OP)[subscript]`
- `(bool OR / AND).bool_or_or`
- `(ternary).attr`
- `(lambda)(arg)`  (happens to work by chance when no further suffix)

Fix surface: the emitter (likely `tyc-emit` precedence table) needs to
treat attribute/call/subscript and the boolean ops as suffix contexts
that require parens around any non-atomic primary.

### E2 — Impl methods reject `T?` parameters  *(CRITICAL)*

`impl X: def foo(self, p: T?)` does **not** accept `None` or a `T?`
value as an argument. The same signature on a free `def` works fine.

Repro (`08-meta-stress/06-nullable-impl-method.ty`):

```python
class API:
    name: str

impl API:
    def fetch(self, cursor: str?) -> int:
        return 0 if cursor is None else len(cursor)

def free_fetch(cursor: str?) -> int:
    return 0 if cursor is None else len(cursor)

def main() -> None:
    let api: API = API(name="x")
    let v: str? = None
    print(free_fetch(v))   # ✅ accepted
    print(api.fetch(v))    # ❌ tyc::nullable_use — "value is `? | None`"
    print(api.fetch(None)) # ❌ tyc::type_mismatch  — "expected `?`, found `None`"
```

The diagnostic also misrenders the parameter type as a bare `?` instead
of `str?` (or `str | None`), suggesting that the resolver attaches a
`Nullable<unknown>` to impl-method params and the comparison falls back
to nominal identity. Surfaced live in
`07-sdk-client/04-pagination-gen.ty` (paginator passing a `cursor: str?`
into a pagination helper).

### E3 — Context-manager pattern broken: `return self` from impl `__enter__`  *(HIGH)*

`impl X: def __enter__(self) -> X: return self` fails type-checking with
`expected X, found __typhon_impl_X`. The synthetic `__typhon_impl_X` is
leaking out of the desugarer into the type-check error surface.

Repro (`08-meta-stress/23-context-mgr-impl.ty`, `02-io-heavy/09-context-manager-custom.ty`):

```python
class Stopwatch:
    label: str
    start: float

impl Stopwatch:
    def __enter__(self) -> Stopwatch:
        self.start = time.monotonic()
        return self          # ❌ expected `Stopwatch`, found `__typhon_impl_Stopwatch`
    def __exit__(self, ...) -> None: ...
```

This blocks the most common context-manager shape in Python (timers,
spans, locks, transactions, …). Workaround would be to box the result
or return `cast(Stopwatch, self)`, but neither is documented.

### E4 — Type alias of `Callable[...]` doesn't unwrap on call  *(HIGH)*

When a parameter has type `Handler` where
`type Handler = Callable[[Req], Resp]`, calling `next(req)` infers the
return type as `Handler` rather than `Resp`.

Repro (`06-api-server/03-middleware.ty`):

```python
from typing import Callable
type Handler = Callable[[Request], Response]

def with_auth(next: Handler) -> Handler:
    def wrapped(req: Request) -> Response:
        return next(req)   # ❌ expected `Response`, found `Handler`
    return wrapped
```

Inlining the `Callable[[Request], Response]` annotation instead of the
alias is likely a workaround, but it defeats the point of the alias.
This makes middleware-style typed pipelines and any FP-like layered
design painful.

### E5 — Operator overload via `__mul__` with mismatched scalar type rejected  *(MEDIUM)*

`impl V: def __mul__(self, scalar: float) -> V` is rejected by the type
checker for `let d: V = a * 5.0`:

```
× type mismatch: expected `Vec2`, found `float`
```

The same code with `__mul__(self, scalar: int)` and `a * 4` works
(`08-meta-stress/26-operator-overload.ty` passes). So operator
overloads via `__mul__` are honoured for some scalar types but the
checker appears to short-circuit to built-in `float.__mul__` rules and
ignore the user-defined method when the right operand is `float`/`int`
in an unexpected order.

Repro (`08-meta-stress/22-dunder-ops.ty`).

### E6 — Exhaustiveness false negatives on sealed unions  *(MEDIUM, R3.12 still open)*

The exhaustiveness checker still misclassifies several legitimate-total
matches as missing a return:

- Recursive sealed: `type Tree = Leaf | Branch` with positional captures
  (`01-syntax-edge/05-sealed-recursive.ty`).
- Five-variant sealed of dataclasses incl. an empty `class Idle: pass`
  variant (`05-agents/03-agent-state-machine.ty` — fixed with explicit
  `case _:`).
- Three-variant tool union with positional captures
  (`04-ai-llm/03-llm-tools-sealed.ty`).

In all cases every variant is matched. The fix is forcing `case _:`
which works but defeats the safety property. The checker likely needs
to walk class-pattern destructuring (positional and keyword) the same
way as bare class-pattern checks.

### E7 — Comptime f-strings rejected even though docs imply string ops are supported  *(MEDIUM)*

The skill doc lists `"upper"`, `"lower"`, … and `+` as comptime-supported,
but the natural form `f"{APP} v{MAJOR}.{MINOR}"` is rejected at
comptime even though every interpolated value is a comptime constant.

Workaround: `str(MAJOR) + "." + str(MINOR)` works.

Repro (`08-meta-stress/21-comptime-fstring.ty`,
`01-syntax-edge/12-comptime-features.ty` after edit).

This is a small surface but it's the very first thing anyone reaches
for when building a comptime version string.

### E8 — Class with field defaults emits useless slot descriptors  *(MEDIUM, R3.2 still open)*

```python
class HTTP:
    GET: str = "GET"
    POST: str = "POST"
```

`HTTP.GET` resolves to `<member 'GET' of 'HTTP' objects>` (a
`member_descriptor`), not `"GET"`. Confirmed at
`08-meta-stress/09-class-const.ty`. The intuitive "namespace of
constants" pattern is silently broken with no diagnostic — this is the
same finding as R3.2 from the prior round.

Either:
- emit class-level constants without `@dataclass(slots=True)` (or as
  `ClassVar`),
- diagnose at check-time when a `class` body contains assignments
  intended as constants and recommend a `frozen` model, or
- support an explicit `class HTTP namespace:` form.

### E9 — `?` operator still rejected inside subexpressions  *(MEDIUM, R3.13 still open)*

Repro (`01-syntax-edge/09-question-subexpr.ty`):

```python
def both(s1: str, s2: str) -> Result[int, str]:
    return Ok(add(parse(s1)?, parse(s2)?))
# ❌ tyc::invalid_question_op (both occurrences)
```

Documented as "lift the inner call to a `let` binding first". The
workaround works but it's heavyweight for short helpers and pushes
users to ditch `?` for nested cases.

### E10 — `for` loop variable rebinding from an outer `let` is rejected  *(LOW papercut)*

`let lbl: str = ...` followed later in the same scope by
`for lbl, count in counts.items():` produces `tyc::immutable_assign`,
because the for-loop variable is treated as reassignment of the
existing `let` binding rather than introducing a new loop-scoped one.

Repros (`03-ml-numpy/05-knn-toy.ty` earlier versions). The doc lists
walrus and `gather:` as exceptions to Rule 2 but doesn't address
`for` targets. Either:
- treat `for` targets as carve-outs (Python doesn't scope them either,
  but the binding is fresh on each iter), or
- recommend renaming, with a hint diagnostic.

Currently the diagnostic correctly *fires*, but the framing ("illegal
re-assignment") doesn't match the intuition for a `for` target.

### E11 — Build environment provisioning is per-invocation  *(LOW DX)*

Every `tyc build` invocation, when `[dependencies]` is non-empty,
spawns `uv sync` and reprovisions `.venv` from scratch. For one-off
`.ty` files under tmp directories (our stress harness, ad-hoc REPL-like
testing) this dominates wall-clock time. A `--no-deps` or
`--reuse-venv` flag would speed iteration.

## Notable working features (highlights)

- **`gather:`** with `@gatherable` callees: clean TaskGroup lowering;
  multi-agent message-passing works through a single gather
  (`05-agents/02-multi-agent.ty`).
- **`go ... -> task`** strong-ref registry holds tasks correctly;
  await on captured handle works (`01-syntax-edge/14-go-tasks.ty`).
- **`Result[T, E]` with `?` and `with`-chain** — both shapes work
  including the `else err:` shape in `04-result-chains.ty`,
  `04-validation-chain.ty`, and the deeper `18-result-deep.ty`.
- **`extend BUILTIN:`** correctly extracts `__typhon_ext_str__slug` and
  rewrites the call site (`15-extend-builtin.ty`).
- **`extend Class:`** merges across multiple blocks
  (`08-meta-stress/17-extend-class.ty`).
- **`model X:`** emits Pydantic with `extra="forbid"`; runtime
  validation surfaces nicely at the boundary
  (`04-ai-llm/05-llm-structured-out.ty`).
- **`@pure` / `@memo` / `@pure(memo=True)`** all work; the six-condition
  enforcement catches `@pure def foo(xs: list[float])` cleanly
  (`01-syntax-edge/13-pure-memo.ty`).
- **`lazy import`** without alias works; `lazy let` at module scope
  caches as documented (`17-lazy-import.ty`, `18-lazy-let-module.ty`).
- **`@dataclass(slots=True)` underneath** doesn't interfere with normal
  field assignment; field validation flows through pydantic for `model`
  (`16-model-pydantic.ty`).
- **PEP 695 `class Box[T]:` + `impl[T] Box[T]:`** with multi-level
  recursion (`02-deep-generics.ty`).
- **Closures over local `mut`** bindings work (`05-event-bus.ty`'s
  `log.append` from inner functions).

## Suggested priorities

1. **E1 (paren-stripping)** — silent wrong-output bug. Single-character
   fix matters most because it lands in real ML/path code.
2. **E2 (impl `T?` params)** — every typed SDK / repository / service
   that has an optional argument hits this. Big DX hit.
3. **E3 (`__enter__` self return)** — context managers are the most
   common impl method beyond `__init__` you don't write.
4. **E4 (Callable alias)** — blocks middleware / decorator patterns and
   is bound to recur with web/server users.
5. **E6 (exhaustiveness false neg)** — undermines the "sealed unions
   give you exhaustive matches" pitch.
6. **E8 (class-const slots)** — silent slot-descriptor leak; either fix
   emit or diagnose at parse.
7. **E5 (`__mul__` float scalar)** — operator overload should resolve
   user impl before built-in scalar rules.
8. **E7, E9** — papercut documentation/parser limitations.
9. **E10** — diagnostic phrasing.
10. **E11** — DX.

## Reproducing

```bash
cd /home/user/Typhon
cd tyc && cargo build --release && cd ..   # if not already built
for f in stress/round-2026-05-20-exploration/0[1-7]-*/*.ty; do
  bash stress/round-2026-05-20-exploration/build_and_run.sh "$f"
done
# Detail on any single case:
SHOW_EMIT=1 bash stress/round-2026-05-20-exploration/build_and_run.sh \
  stress/round-2026-05-20-exploration/08-meta-stress/25-paren-emit-suite.ty
```
