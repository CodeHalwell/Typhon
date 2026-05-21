# Typhon — Stress Round 2026-05-21

- Compiler: `tyc 0.2.5`, built from this commit (`cargo build --release`).
- Python: `/usr/bin/python3.13` (3.13.12).
- Total cases authored: **81 `.ty` programs** across 10 categories.
- Pass-on-build (after fixing my own typing mistakes): **65/81**.
- Of the 16 build/run failures: **9 are deliberate diagnostic probes** in
  `10-error-quality/` whose purpose is to fail and let me read the
  message; the remaining **7 are real compiler bugs** (with B5 surfacing
  in `21-narrowing-loop`, `22-narrowing-while-only`, and `20-self-type`).

## Layout

```
01-language-edge/   (20)  let/mut, generics, sealed unions, Result, gather/go,
                          comptime, lazy, pipes, frozen, operator overload, etc.
02-io/              ( 8)  file IO, JSON, CSV w/ unicode, sqlite, regex, pathlib,
                          asyncio queues, threading + locks
03-ml-numpy/        ( 4)  pure stats, numpy matmul/broadcasting, gradient descent,
                          k-NN classifier
04-ai-llm/          ( 5)  mock LLM client, generator-based streaming, tool dispatch
                          via sealed union, structured-output JSON validation,
                          mini-RAG cosine search
05-agents/          ( 3)  ReAct-style trace, multi-agent fan-out under asyncio,
                          topological planner-executor
06-api/             ( 3)  mini router, validation chain returning Result, middleware
                          stack with continuation-passing
07-sdk/             ( 3)  retry+rate-limit, cursor paginator, event bus
08-meta-stress/     (23)  targeted compiler probes — parens, classmethod, iterators,
                          context managers, closures, default/var args, recursive
                          generics, narrowing, exceptions, etc.
09-perf/            ( 2)  memoised fib, 1M-element comprehension
10-error-quality/   (10)  intentionally-broken programs whose diagnostics are read
                          and graded
```

`run_one.sh <file>` builds and runs a single case; `run_all.sh` runs the
whole suite and writes `run_all.log`. A sidecar `<name>.deps` file (one
package per line) injects `[dependencies]` into the generated `typhon.toml`
(used to pull in `numpy`, etc.).

---

## New bug & limitation findings

Codes use **B** = real compiler bug, **L** = documented limitation worth
revisiting, **D** = diagnostic / DX, **T** = tooling.

### B1 — `tuple[T, ...]` variadic tuple type rejects tuple literals  *(High)*

Repro: `08-meta-stress/11-tuple-variadic-bug.ty`.

```python
let xs: tuple[float, ...] = (1.0, 2.0, 3.0)   # tyc::type_mismatch
let ys: tuple[float, ...] = ()                # tyc::type_mismatch
```

The diagnostic prints the expected type as `tuple[float, ?]` (note the
literal `?`), suggesting the variadic-marker `...` is being rendered as a
placeholder — the unifier never recognises `tuple[T, ...]` as homogeneous.
This is the documented spelling in the cheatsheet and language docs (see
the "Collections" table) so it should match. Today users must spell it as
`list[float]`, which costs hashability and loses positional indexing.

### B2 — Recursive `type` alias rejected as a cycle  *(High)*

Repro: `08-meta-stress/12-recursive-type-alias-bug.ty`.

```python
type JSON = None | bool | int | float | str | list[JSON] | dict[str, JSON]
# tyc::alias_cycle  +  tyc::type_mismatch on every use
```

A self-referencing type alias through `list[…]` / `dict[str, …]` is a
classic pattern — every typed-JSON, AST, tree-of-X type needs it. Python's
own typing module accepts the equivalent via `TypeAlias` (pre-PEP-695) and
the new `type X = …` syntax. Workaround today is `dict[str, object]`,
which throws away all the typing.

### B3 — `return` inside a generator function is type-checked as value return  *(Medium)*

Repro: `08-meta-stress/13-generator-return-bug.ty`,
`07-sdk/02-paginator.ty` (original form).

```python
def stop_early(n: int) -> Iterator[int]:
    for i in range(n):
        if i > 5:
            return        # tyc::type_mismatch: expected Iterator[int], found None
        yield i
```

In CPython a bare `return` inside a generator is `StopIteration`, not
a value return. The checker is reading the surrounding function as a
regular `def Iterator[int]:` and demanding the return statement produce an
`Iterator[int]`. Generator-detection (any `yield` in the body) needs to
flip the return-statement validator.

### B4 — `class!` Exception subclass synthesises arg-less `__init__`  *(High)*

Repro: `08-meta-stress/14-exception-class-bug.ty`.

```python
class! AppError(Exception):
    pass

raise AppError("hello")        # TypeError: __init__ takes 1 positional, got 2
```

Emitted:

```python
class AppError(Exception):
    def __init__(self) -> None:
        super().__init__()
    pass
```

The synthesised init drops the message arg. For an exception in particular
(but for any raw-class with a non-trivial parent more broadly), the emit
should generate `def __init__(self, *args, **kwargs): super().__init__(*args, **kwargs)`
when the body has no fields. Workaround is to declare an explicit
`message: str` field and `raise AppError(message="…")` (kwarg only), which
loses the conventional `raise AppError("msg")` ergonomics every Python
programmer expects.

### B5 — Flow narrowing fades inside `while x is not None:` loop body  *(High)*

Repro: `08-meta-stress/21-narrowing-loop.ty`,
`08-meta-stress/22-narrowing-while-only.ty`,
`08-meta-stress/20-self-type.ty`.

```python
def sum_list(head: Node?) -> int:
    mut total: int = 0
    mut cur: Node? = head
    while cur is not None:
        total = total + cur.value     # tyc::nullable_use on cur.value
        cur = cur.next                # tyc::nullable_use on cur.next
    return total
```

Inside the body, `cur` *should* be narrowed to `Node` for the whole
iteration until the (last) assignment that rebinds it. Today the checker
drops narrowing on `mut` bindings the moment the surrounding statement
mutates *anything*: after `total = total + cur.value` the second use of
`cur.value` on the same line is fine, but the next statement's use is
not. Even rebinding the value with `let n: Node = cur` immediately inside
the loop fails with `tyc::type_mismatch` because narrowing doesn't reach
the assignment site.

This breaks the most common iterator pattern. The recursive form
(`08-meta-stress/23-narrowing-recursive.ty`) works fine because the
parameter never rebinds.

### B6 — `tyc migrate` produces unparseable forward-reference annotation  *(Medium)*

Repro: feed any `.py` with `parent: Optional["Item"] = None` through
`tyc migrate`. Emitted:

```python
parent: "Item"? = None      # tyc::parse — Got unexpected token ?
```

The migrated `.ty` doesn't even build (`10-error-quality/09-migrate-output.ty`).
The correct rewrite is `parent: "Item?" = None`.

### B7 — `tyc migrate` doesn't rewrite `Union[T, None]` → `T?`  *(Medium)*

Same input. `Optional[T]` is rewritten but `Union[T, None]` is not, and
`from typing import Union` is left dangling. This is documented as
"conservative" — but `Union[T, None]` is identical to `Optional[T]` and
hits real-world code at the same rate.

### B8 — `extend list[int]:` (parametric) silently mis-targets `list`  *(Low)*

Repro: `01-language-edge/16-extend-builtin.ty` (initial form).

```python
extend list[int]:                  # diagnostic talks about `impl list:` instead
    def total(self) -> int: ...
```

Diagnostic claims the user wrote `impl list:`. Either accept parametric
extends or surface a `extend on parameterised types is not supported yet`
error.

### B9 — `tyc fmt` doesn't normalise `.ty` whitespace beyond stripping  *(Medium)*

Feeding `def f(  x: int ,y:int   ) -> int:` through `tyc fmt` returns
`def f(x: int,y:int) -> int:` — the leading/trailing spaces inside the
paren are dropped but missing spaces around `,`, `:`, `=`, `->` stay
missing. Ruff is on PATH; the docs say `tyc fmt` is "wrapped in `ruff
format`", but ruff doesn't understand `let`/`mut`, so the wrapper falls
back to a partial whitespace pass.

`tyc build` *does* produce ruff-formatted Python (correctly). So the
deliverable is clean; the source file isn't.

### B10 — Pattern-binding names collide with outer `let` bindings  *(Medium DX)*

Repro: `08-meta-stress/15-pattern-name-scope.ty`,
`05-agents/01-react-agent.ty` (initial form).

```python
let value: int = 99
match b:
    case Wrap(value):              # tyc::immutable_assign — `value` is a let
        print(value)
```

Pattern variables in `case Foo(name):` are conceptually new bindings, not
re-assignments. Treating them as the same name as an enclosing `let` is
consistent with Rule-2 / function scope but is extremely surprising in
practice — every Rust/OCaml/Scala programmer expects pattern bindings to
introduce fresh names. Either (a) introduce a per-`case` scope, or (b)
upgrade the diagnostic to `tyc::pattern_shadows_outer` with a clear
rename hint.

### B11 — VM repr disagrees with CPython repr for `Result`  *(Low)*

VM prints `Ok(20)`; CPython prints `Ok(value=20)`. Causes `tyc run` and
`tyc run --compile` to diverge in their stdout, which the `tyc-vm` doc
explicitly warns about, but it's worth surfacing for screenshot-driven
docs and test fixtures.

### B12 — `tyc explain --list` is referenced but not implemented  *(Low DX)*

When `tyc explain not_a_real_code` fails, the help text reads:

> Run `tyc explain --list` (not yet implemented) or see https://typhon.dev/lang/diagnostics

The "not yet implemented" disclaimer is in the user-visible error message.
Either land the subcommand or rewrite the suggestion.

### B13 — REPL prompt UX prints stacked `>>>` for empty lines  *(Low DX)*

Hammering newlines or pasted multi-line code produces output like
`>>> >>> >>> 6`. The block-end-on-blank-line behaviour is documented but
the visible state of the prompt makes a paste look broken.

### B14 — Pydantic emission requires `pydantic` runtime dep, not declared by `tyc build`  *(Medium T)*

Any `model X:` in source generates `from pydantic import BaseModel, ConfigDict`
in the emitted Python. If the project doesn't list `pydantic` in
`[dependencies]`, `tyc build` succeeds (because uv-sync only resolves
declared deps) and execution then fails with `ModuleNotFoundError`. The
compiler knows it emitted a Pydantic class — it could either inject the
dep into the synthesised `pyproject.toml` or warn during build.

---

## Confirmed working (the happy path is broad)

Categories that pass cleanly across all probes, often handling edge cases
gracefully:

| Area | Notes |
|---|---|
| `let` / `mut` semantics | Function-scoped strictness is consistent; reassignment, walrus, nonlocal all work. |
| Sealed unions + `match` | Field-pattern, named-pattern, guards, recursive sums — all fine. Exhaustiveness diagnostic is precise. |
| Generics (PEP 695) | `def f[T]…`, `class Box[T]:`, inference through `?` correctly produces `T?`. |
| `Result[T, E]` + `?` propagation | Including 3-deep `with`-chains and `else err:`. Stack traces don't get polluted. |
| `gather:` + `go` | TaskGroup lowering works; `go f() -> task` captures the handle. |
| `pipes` (`\|>`) | Chains 6 deep evaluate left-to-right and don't double-apply args. Parens around the LHS work. |
| `comptime let` | Inlined as literals; `comptime def` is callable from other comptime contexts. |
| `lazy let` (module) | One-shot evaluation under sentinel cache. |
| `impl` + `extend` merge | Multiple `impl` blocks for the same class merge correctly. |
| `class! Foo(SomeBase):` (with fields) | Raw class for ML/framework bases works (PyTorch-style). |
| Numpy, sqlite, pathlib, csv | Standard-library interop is clean. The implicit `unsafe:` for `np.array(...)` is required. |
| Pure / `@memo` | Inferred purity, caching, recursive fib OK. The 6-rule rejection is correct. |
| Async + queues | `asyncio.Queue[int]`, `await`, `gather:` desugars cleanly. |
| Generator iter-protocol | `Iterator[T]` via `def __iter__(self) -> Iterator[int]: yield …` works. |
| Closures + nonlocal | `make_counter() → inc()` captures correctly. |
| Context managers | `__enter__`/`__exit__` impl methods. |
| Operator overloading | `__add__`, `__sub__`, `__mul__`, `__eq__` route through Python. |
| Complex f-strings | `f"{n:05d}"`, `f"{pi=:.4f}"` self-doc form. |
| Walrus `:=` | Type narrows the bound name as a `let`; rebind requires `mut`. |
| 1 M element comprehension | Emit and runtime both fast. |
| VM (`tyc run`) | Handles all pure-Typhon programs in this suite. `Ok(v)` vs `Ok(value=v)` is the one visible difference. |

---

## Suggestions for the docs / language

Roughly ranked by how often I tripped on them:

1. **The cheatsheet shows `tuple[float, ...]`** as a homogeneous tuple but the
   checker doesn't actually accept tuple literals there. Either fix B1 or
   change the docs to spell it as `list[float]` / `Sequence[float]` for the
   common case.
2. **The Exception subclass story is a footgun.** Document the
   recommended pattern explicitly:
   - Use `class!` to get a real subclass.
   - Provide a `message: str` field.
   - Raise with kwargs: `raise AppError(message="…")`.
   - …or fix B4 so the conventional positional `raise AppError("…")` works.
3. **`tyc migrate` needs a "your output may not build" disclaimer** in
   the CLI help. Today it prints `migrated 1 file(s)` and returns 0 even
   when the produced `.ty` fails `tyc check`. Recommend chaining
   `tyc migrate … && tyc check src/` in the docs.
4. **Recursive type aliases (B2) deserve at least a `not yet supported`
   diagnostic** instead of the current `alias cycle` + per-use mismatch
   firehose. Today a tree type produces dozens of cascading errors that
   bury the actual cause.
5. **Loop-body narrowing (B5) is so common** that adding even a
   conservative "narrowing survives until the next assignment to the
   narrowed name in this block" would unlock 90% of the iterator
   patterns. Until then, the docs should explicitly recommend recursion
   or the `let n: Node = cur; …` pattern (which today *also* fails).

---

## Suggestions for the toolchain

- **`tyc build` should auto-inject `pydantic` into the synthesised
  `pyproject.toml`** whenever it emits `BaseModel`/`ConfigDict`. Otherwise
  every project with a `model` block fails at first run with a confusing
  `ModuleNotFoundError` (B14).
- **`tyc fmt` on `.ty` source needs a real formatter** (B9). The current
  output reads as if `ruff format` was skipped — which is in fact what's
  happening, because ruff doesn't speak `let`/`mut`. A Typhon-aware
  formatter is the only way out; the emit-time `ruff format` only catches
  the output `.py`, not source readability.
- **`tyc explain --list`** — wire it up, or drop the mention from the
  not-found error (B12).
- **`tyc trace` falls back gracefully when given a Python file that
  doesn't have a `.py.map`** — but when given a file that was modified
  after the map was written (e.g. extra lines appended), it maps to
  out-of-range source lines silently. Detecting that the appended line
  goes beyond the mapped range and saying so would help.
- **VM error message for unsupported stdlib imports** is good ("Run with
  `tyc run --compile`"). Could also list which stdlib modules *are*
  supported so users know whether to bother with `tyc run` at all.

---

## What I left untested

- Free-threaded mode (3.13t / 3.14t). The toolchain accepts the config
  but I don't have a 3.13t interpreter on this image.
- `tyc profile` + `pgo-memoise` round-trip.
- `tyc debug` with a real `--break <file>:<line>` interactive session
  (the harness is non-interactive).
- The full VS Code extension flow.
- `tyc stubtest` against a `.dty` of a non-trivial Python package.
- Multi-file projects with `.dty` stubs of internal modules.
- A `class!` inheriting a real framework class (e.g. `nn.Module`) under
  PyTorch — requires the heavyweight `torch` install.

These are good targets for a follow-up round.
