# Stress Round 2026-05-22 — Findings

Branch: `claude/typhon-library-testing-ch1yz`
Compiler: `tyc 0.3.0` built from source on this branch.
Python: CPython 3.13.12 (venv at `/tmp/typhonvenv` with pydantic + numpy).

Severity legend:
- **CRITICAL**: silent wrong output or runtime crash on documented happy path
- **HIGH**: documented feature unusable / forces awkward rewrites
- **MEDIUM**: feature works with workaround
- **LOW**: UX / DX / docs nit

---

## Summary

| Code | Severity | Surface | One-liner |
|---|---|---|---|
| **N5** | CRITICAL | emit | `not (a or b)` parens stripped → wrong-output |
| **N6** | CRITICAL | emit | `not (x if c else y)` parens stripped → wrong-output |
| **N9** | CRITICAL | tyc-vm | `match` arm writes don't propagate to outer scope |
| **N1** | HIGH | desugar | `freeze let` chokes on multi-line dict/list literal |
| **N2** | HIGH | desugar | `?` inside list comprehension hoists past `for` binding |
| **N10** | HIGH | tyc run | VM skips static checker — `NameError` instead of `tyc::unknown_name` |
| **N11** | HIGH | tyc migrate | output for code with generics/methods doesn't build |
| **N13** | HIGH | types | `match self.<field>:` false-positive `missing_return` |
| **N4** | MEDIUM | resolve | no typed multi-let tuple unpacking form |
| **N8** | MEDIUM | desugar | duplicate-method silently merged on `impl`+`extend` collision |
| **N3** | LOW | comptime | `str.join` not in the supported method set |
| **N7** | LOW | diagnostics | `tyc::type_mismatch` help text wrong-direction for newtypes |
| **N12** | LOW | emit | `from __future__ import annotations` emitted twice when user wrote it |

13 findings filed. 3 CRITICAL (two silent-wrong-output, one VM mis-scoping),
5 HIGH, 2 MEDIUM, 3 LOW.

The CRITICAL findings are the most worrying: each compiles cleanly without
any warning and produces wrong runtime output. N5 surfaced organically
during the `05-agents/01-react-agent.ty` test — a real-world calculator-
tool guard `not (ch.isdigit() or ch in " +-*/()")` malfunctioned. N9 broke
the VM result-accumulation path the moment any program tried to sum
matched values.

---

## Open findings

### N5 — `not (X or Y)` parens stripped on emit (CRITICAL — silent wrong output)

**Repros**: `01-language-edge/19-paren-not-or.ty`, `21-not-paren-strip.ty`,
`05-agents/01-react-agent.ty`, `04-tool-dispatch-sealed.ty` (real-world hit).

Source:
```python
def check(ch: str) -> bool:
    return not (ch.isdigit() or ch in " +-*/()")
```

Emitted:
```python
def check(ch: str) -> bool:
    return not ch.isdigit() or ch in " +-*/()"
```

Python's operator precedence: `not` = 5, `or` = 3, `and` = 4. The two
forms differ:
- Source: `not (X or Y)` ≡ `(not X) and (not Y)` (De Morgan).
- Emitted: `(not X) or Y`.

Variants confirmed:
- `not (a or b)` → `not a or b` (broken)
- `not (a and b)` → `not a and b` (broken)
- `not (x == 0 and y == 0)` → `not x == 0 and y == 0` (broken)

**Suspected location**: `tyc/crates/tyc-emit/src/printer.rs:903`
(`Expr::UnaryOp` arm). The arm prints `"not "` + operand without
consulting `expr_precedence`. The fix is to mirror the `BinOp` left/
right precedence wrap: if `expr_precedence(&u.operand) < 5` (for
`Not`) — i.e. operand is `BoolOp`, `Lambda`, `If`, `Named` — wrap in
parens. Arithmetic unaries (`USub`/`UAdd`/`Invert`, prec 13) need a
similar wrap for sub-precedence operands.

Existing `expr_precedence()` already has the right table:
```rust
Expr::BoolOp(b) => match b.op { Or => 3, And => 4 },
Expr::UnaryOp(u) => match u.op { Not => 5, ... => 13 },
```

so a small `if expr_precedence(&u.operand) < self_prec { wrap }`
change should land both N5 and N6.

---

### N6 — `not (X if C else Y)` parens stripped (CRITICAL — silent wrong output)

**Repro**: `08-meta-stress/01-paren-precedence-extras.ty` (`t_ternary_in_not`).

```python
def t_ternary_in_not(a: int) -> bool:
    return not (True if a > 0 else False)
```

Emitted:
```python
return not True if a > 0 else False
```

For `a = -5`:
- Source: `not (True if False else False)` = `not False` = `True`.
- Emitted: `(not True) if False else False` = `False`. WRONG.

Same root cause as N5 — `Expr::If` has precedence 2 in
`expr_precedence`, `Not` is 5, so 2 < 5 needs parens. The fix in
`printer.rs` covers N5 and N6 simultaneously.

---

### N9 — VM `match` arm doesn't propagate writes to outer scope (CRITICAL)

**Repros**: `08-meta-stress/19-vm-match-augmented.ty`,
`19d-vm-match-single.ty`, `19e-vm-match-class.ty`, `19f-vm-match-let.ty`.

```python
def main() -> None:
    let r: Result[int, str] = Ok(42)
    mut total: int = 0
    match r:
        case Ok(v):
            total = v
        case Err(_):
            pass
    print(f"total = {total}")
```

- `tyc run` (VM)      → `total = 0`     ← WRONG
- `tyc build` + python → `total = 42`   ← correct

The VM treats each `match` arm as a fresh scope: writes to `total`
inside `case Ok(v):` don't escape to the function body. Reproduces with:
- augmented (`total += v`) and explicit (`total = v`) assignment,
- single `match` (no loop) and `match` inside `for`,
- `Result[T, E]` and *any* user-defined sealed union.

Without `match` (using `if isinstance(r, Ok):`) the VM behaves
correctly. The bug is localised to the VM's `match`-statement scope
handling — the arm body is presumably executed in a child env that
isn't merged back into the parent on exit.

`tyc run` is documented as the default execution mode (`docs/vm.md`).
Anyone writing a `match`+accumulator (every aggregator, every state
machine, every Result-walker) gets the wrong answer.

**Suspected location**: `tyc/crates/tyc-vm`, the `match` statement
visitor. The fix is to use the same env reference for the arm body
as for the enclosing block, rather than pushing a fresh frame.

---

### N1 — `freeze let` chokes on multi-line dict / list literal (HIGH)

**Repro**: `01-language-edge/03-freeze-let-mutation.ty` (multi-line
dict). `03b-freeze-let-singleline.ty` proves the single-line form works.

```python
freeze let CONFIG = {
    "ports": [8080, 8081, 8082],
    "hosts": {"primary": "a.example.com"},
}
```

Emitted (parse-fails):
```
CONFIG = __typhon_freeze__({)
    "ports": [8080, 8081, 8082],
    ...
```

The desugar pass injects `__typhon_freeze__(...)` around the RHS but
only closes the call on the binding's *first* line; the multi-line
dict body leaks out unbalanced.

**Suspected location**: `tyc-desugar` `freeze let` lowering. Likely a
text-level wrap rather than an AST-level wrap.

---

### N2 — `?` inside list comprehension hoists past `for` binding (HIGH)

**Repro**: `01-language-edge/05-result-question-in-comp.ty`.

```python
def collect(items: list[str]) -> Result[list[int], str]:
    let xs: list[int] = [parse(s)? for s in items]
    return Ok(xs)
```

Emitted:
```
__typhon_qi_0__ = parse(s)        # ← s undefined here
if isinstance(__typhon_qi_0__, __typhon_Err__):
    return __typhon_qi_0__
xs: list[int] = [__typhon_qi_0__.value for s in items]
```

The `?` rewrite hoists the call out *above* the comprehension entirely,
referencing `s` before the `for s in items` binds it. The resulting
diagnostic is misleading: "cannot find 's' in scope" points at user
code, not at the hoisted lift.

Either:
- **Reject** with `tyc::invalid_question_op`: "the `?` operator
  cannot appear inside a comprehension — rewrite as a loop", or
- **Lower correctly** by transforming the comp into a for-loop that
  threads the `Err` short-circuit through.

The current behaviour combines silent rewrite + misleading error.

---

### N10 — `tyc run` (VM) doesn't run the static checker first (HIGH)

**Repro**: `08-meta-stress/19g-vm-no-static-check.ty`.

```python
def main() -> None:
    print(undefined_name)
```

- `tyc check` catches `tyc::unknown_name` cleanly.
- `tyc run` skips check and crashes with `NameError` at runtime.

Same gap for `tyc::no_block_shadow` (`19f-vm-match-let.ty`): the VM
accepts code that the static analyser rejects, then runtime-crashes
with `NameError`. `tyc run` should at minimum run the checker first
so all the Typhon-specific diagnostics (`unsafe_value_leak`,
`blocking_in_async`, `pattern_shadows_outer`, …) are surfaced
consistently with `tyc build`.

---

### N11 — `tyc migrate` output for code with generics/methods doesn't build (HIGH)

**Repro**: `/tmp/migrate_hard.py` (captured under
`stress/round-2026-05-22/build/migrate_hard/`).

Input Python (typed, modern):
```python
from typing import TypeVar, Generic, Optional, List
T = TypeVar("T")

class Container(Generic[T]):
    def __init__(self, items: List[T]) -> None: ...
    def first(self) -> Optional[T]: ...
```

`tyc migrate` produces:
- `class! Container(Generic[T]):` with `def first(self) -> T?:` still
  **inside** the class body → fires `tyc::method_in_class_body` on
  build.
- `TypeVar` still imported from `typing` → fires
  `tyc::typevar_import_rejected` on build.
- `Generic[T]` base kept — should rewrite to PEP 695 `class C[T]:`.

The migrate→check→build pipeline is not internally consistent for the
exact Python idioms most third-party code uses.

**Fix surface**: extend the migrate rewrites to:
1. Drop `TypeVar` declarations and rewrite class/function generic
   bases (`Generic[T]` → PEP 695 `class C[T]:`).
2. Lift `def` methods out of `class` bodies into companion
   `impl C:` blocks.
3. Strip the now-orphan `Generic` / `TypeVar` imports.

---

### N13 — `match self.<field>:` with exhaustive arms triggers false-positive `missing_return` (HIGH)

**Repros**: `01-language-edge/29-match-arm-with-self-write.ty`,
`30-match-on-self-field.ty`, `30b-match-local-works.ty` (workaround).
Surfaced organically in `07-sdk/03-circuit-breaker.ty`.

```python
type Status = Open | Closed
class Open: since: float
class Closed: label: str

class Foo:
    state: Status

impl Foo:
    def check_self(self) -> str:
        match self.state:                  # ← match on instance field
            case Open(_): return "open"
            case Closed(_): return "closed"
```

→ `tyc::missing_return — function `check_self` is missing a return on
some paths`.

The sealed union has two variants and both arms return. The
`tyc::non_exhaustive_match` diagnostic doesn't fire — only the
missing-return one does, which is consistent with the analyser failing
to see exhaustiveness on `self.field` specifically. Binding the field
to a local first satisfies the analyser:

```python
def check_local(self) -> str:
    let s: Status = self.state          # ← local
    match s:
        case Open(_): return "open"
        case Closed(_): return "closed"
```

This bites the most common pattern in the docs: a class with a
sealed-union state field driving behaviour via `match`.

---

### N4 — Multi-let initialiser pattern blocked (MEDIUM — ergonomics)

**Repro**: `03-ml-numpy/02-knn-from-scratch.ty` (initial form).

```python
let values: np.ndarray
let counts: np.ndarray
values, counts = np.unique(votes, return_counts=True)
```

→ `tyc::missing_initialiser` on the two declarations.

`let (a, b) = expr` and bareword `let a, b = expr` work, but neither
allows declaring types per element. For ML code that often unpacks
mixed-type tuples and wants the documentation value of named types per
element, this is awkward.

**Workaround** (in tree): bind to a single `let pair = …` and index
`pair[0]` / `pair[1]`, but that loses the typed-unpacking ergonomics.
The cleaner fix is a typed tuple-unpacking form:
```python
let (values: np.ndarray, counts: np.ndarray) = np.unique(votes, return_counts=True)
```

---

### N8 — duplicate-method silently merged when defined in both `impl` and `extend` (MEDIUM)

**Repro**: `08-meta-stress/10-impl-extend-conflict.ty`.

```python
class Box:
    value: int

impl Box:
    def get(self) -> int:
        return self.value

extend Box:
    def get(self) -> int:
        return self.value * 2
```

Emitted Python ends up with **two `def get`** in the class body —
Python silently takes the last one. Result is `10` (the `extend`
version wins) with no diagnostic.

The `tyc::duplicate_class` diagnostic exists for the parallel case;
this should fire a sibling `tyc::duplicate_method` with help text
pointing at both definitions and asking the user to delete or rename
one.

---

### N3 — comptime `str.join` not supported (LOW)

**Repro**: original form of `01-language-edge/11-comptime-stress.ty`.

```python
comptime let JOINED: str = ",".join(TAGS)
```

→ `tyc::comptime — comptime str method 'join' is not supported`.

The diagnostic lists supported methods (`upper/lower/strip/lstrip/
rstrip/replace/startswith/endswith/split`) — `join` is the natural
pair of `split`, used in any "comma-separated env var" scenario,
and would be trivial to add to the sandbox.

---

### N7 — `tyc::type_mismatch` help text wrong-direction for `newtype` violations (LOW)

**Repro**: `09-error-quality/04-newtype-violation.ty`.

```python
newtype UserId = int
def fetch_user(uid: UserId) -> str: ...
def main() -> None:
    print(fetch_user(42))   # int → UserId
```

Diagnostic:
```
× type mismatch: expected `UserId`, found `int`
  help: change the value, or update the annotation to `int`
```

The help text suggests "update the annotation to `int`" — the opposite
of what a newtype user wants. The actual fix is `fetch_user(UserId(42))`.

The skill / cheat sheet docs separately claim this case fires
`tyc::newtype_violation`; the source code (`tyc-types/src/lib.rs:6649`)
confirms `newtype_violation` only fires inside the constructor-arg
case (`UserId("seven")`). `docs/diagnostics/newtype_violation.md` is
the canonical reference; the cheat sheet should be tightened to match.

**Fix**: when `expected` is a `newtype` whose base unifies with
`actual`, swap help to "wrap with `UserId(<value>)`, or change the
annotation to `int` if the nominal type isn't needed here."

---

### N12 — `from __future__ import annotations` emitted twice when user wrote it (LOW)

**Repro**: `01-language-edge/22-from-future-imports.ty`.

User-authored:
```python
from __future__ import annotations
```

Emitted:
```python
from __future__ import annotations
from __future__ import annotations
```

Python tolerates the duplicate, but the emit pass should deduplicate
(or skip the injection when the user has already imported it).

---

## Verified-working surface

The campaign also exercised — without finding new bugs — every documented
language feature:

| Area | Tests | Notes |
|---|---|---|
| `let` / `mut` / walrus interplay | `07-walrus-let-mut-interplay.ty` | mut type-change correctly rejected (`05-mut-rebind-types.ty`) |
| Newtype same-base, cross-newtype | `01-deep-newtype-nesting.ty`, `02-newtype-mismatch.ty`, `17-exhaustive-with-newtype.ty` | both directions handled |
| Freeze let (single-line) | `03b-freeze-let-singleline.ty` | deep_freeze blocks dict/list/set mutation |
| `pub` synthesis of `__all__` | `04-pub-export-list.ty` | covers `let`, `class`, `def`, `model` |
| Generic class + `impl[T]` + `def[U]` | `06-impl-method-with-type-param.ty` | bidirectional inference works |
| Sealed union (recursive Tree) | `08-sealed-recursive-tree.ty` | recursion type-checks |
| Pipe variants | `09-pipe-with-method-call.ty`, `12-pipe-into-method-receiver.ty`, `23-multiline-pipe.ty`, `31-deeply-nested-pipes-args.ty` | multiline pipe works under `( )`, not `\` |
| Async + gather + go | `10-async-deeply-nested.ty`, `13-go-with-generic.ty`, `15-async-context-manager.ty` | TaskGroup and best-effort variants |
| Comptime (`def`, recursive) | `11-comptime-stress.ty` | recursive `comptime def` works |
| `extend BUILTIN:` | `12-extend-builtin-str.ty`, `07-extend-method-on-builtin-elemtype.ty` | str / list both work |
| Structural interface | `13-interface-conformance.ty` | two interfaces via separate impl blocks |
| Nullable narrowing | `14-deep-nullable-narrowing.ty` | guard / is None / chained |
| `?` mid-expression | `15-question-in-arg-position.ty` | works for args, tuples, lists |
| Self-referential class | `06-circular-self-ref.ty` | `"Node?"` shorthand inside quotes |
| Tuple unpacking with `let` | `18-tuple-let-destructuring.ty` | both `let (a, b)` and `let a, b` work |
| `match` exhaustiveness | `25-match-all-arms-return.ty` | works on locals |
| `pure` / `memo` decorators | `08-pure-memo-mixed.ty` | combined form `@pure(memo=True)` |
| F-string nesting / spec | `04-fstring-edge.ty` | nested f-strings, format spec, walrus, conversion `!r` |
| Custom context manager | `05-context-manager-custom.ty` | `__enter__`/`__exit__` annotated |
| SQLite roundtrip | `03-sqlite-orm.ty` | manual ORM with `dict.get` narrowing |
| Pathlib walking | `02-pathlib-walking.ty` | `Path.iterdir`, `Path.stat()` |
| numpy operations | `01-numpy-ops.ty`, `02-knn-from-scratch.ty`, `03-linear-regression.ty`, `04-tensor-batched-ops.ty` | einsum, broadcasting, softmax |
| Pydantic models | `02-structured-output-validation.ty` | `model_validate_json`, ConfigDict |
| Streaming parser | `03-streaming-parser.ty` | stateful state machine on string chunks |
| BPE token counter | `04-token-counter-bpe.ty` | tokens / merges / iteration |
| RAG mock (tf-idf) | `05-rag-mock.ty` | tokenize/index/search end-to-end |
| Prompt templating | `06-prompt-templating.ty` | slot validation via Result chain |
| ReAct agent | `01-react-agent.ty` | sealed-union step trace |
| Workflow state machine | `02-state-machine.ty` | sealed-union state transitions |
| Multi-agent coord | `03-multi-agent.ty` | gather + manual asyncio.gather |
| Tool dispatch | `04-tool-dispatch-sealed.ty`, `05-tool-dispatch-fixed.ty` | request/response sealed unions |
| Mini HTTP router | `01-mini-router.ty` | method+path matching with params |
| Validation pipeline | `03-validation-pipeline.ty` | `with`-chain Result composition |
| GraphQL-style resolver | `04-graphql-resolver.ty` | nested per-field lookups |
| Cursor paginator | `01-paginator.ty` | generic class `Paginator[T]` |
| Typed event bus | `02-event-bus.ty` | sealed-union events + subscribers |
| Circuit breaker | `03-circuit-breaker.ty` | state machine + cooldown timing |
| Diagnostics that fire correctly | `09-error-quality/02..06` | blocking_in_async, resource_not_managed, div_by_zero_literal, type_mismatch, non_exhaustive_match all surface as documented |

---

## Suggested follow-ups (in order of yield)

1. **Fix N5/N6** (single emitter change). One condition in
   `printer.rs` `Expr::UnaryOp` arm closes both CRITICAL silent-wrong-
   output bugs.
2. **Fix N9** (VM match scope). The bug is silent and affects the
   default execution mode; landing it returns the VM to the
   "byte-identical stdout vs compile" parity the docs promise.
3. **Wire N10** (VM runs check first). Tightens the trust boundary
   between `tyc run` and `tyc build`. Likely a one-call-site change.
4. **Fix N1** (`freeze let` multi-line). AST-level wrap instead of
   text-level wrap.
5. **Targeted diagnostic for N2** (`?` in comp). Either reject or
   lower; current state is "silent rewrite + misleading error".
6. **Improve N13** (`match self.<field>:` flow). Probably a missing
   case in the `match` exhaustiveness check that only handles `Name`
   subjects. Extending to `Attribute` on a typed receiver would close it.
7. **Polish N3, N7, N8, N11, N12** as low-priority cleanup.
