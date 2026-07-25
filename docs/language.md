# Language Design

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Each section covers what the feature does, how it desugars to Python, and what the checker must enforce.

## The eight rules every Typhon program follows

Typhon looks like Python, but a handful of rules diverge on purpose. These are the
rules behind almost every "but the same code works in Python" surprise. If you
internalise only this section, you can already read and write most Typhon:

1. **Every parameter and return type is annotated.** There is no implicit-`Any` fallback; a sync function that returns nothing still needs `-> None`.
2. **Local bindings declare `let` or `mut`.** `let` is immutable, `mut` is rebindable. Module-level assignments default to `let`; inside a function the keyword is mandatory.
3. **`T` cannot hold `None`.** Use `T?` (sugar for `T | None`) when a value is optional, and narrow it (`is None`, `guard`, early return, `match`) before use.
4. **Methods live in `impl` blocks, not in `class`.** Write `impl Foo:` with explicit `self`. For an ordinary `class` / `model`, the constructor is generated and a hand-written `__init__` is rejected; the raw-class escape hatches (`class!` and `plain class`) deliberately keep your own `__init__`.
5. **`Any` only enters through `unsafe:` or `.dty` stubs.** Re-assert a concrete type at the boundary; for a one-off value, `EXPR as! TYPE` is the sound one-liner.
6. **`match` on a sealed union must be exhaustive.** Add a variant and every `match` site errors until you handle it — no silent fall-through.
7. **Errors flow as `Result[T, E]`, not exceptions.** `Ok`/`Err` and the `?` operator make failure visible in signatures; bridge to exceptions only at library boundaries.
8. **Declare-only `let NAME: T` must be definitely assigned** before it's read — the first assignment on every non-diverging path is its initialiser.

The rest of this document is the detail under these rules. The
[cheat sheet](cheatsheet.md) (also `tyc cheatsheet`) is the 30-second version.

## Type system

### Non-nullable by default

A plain `T` forbids `None`; `T?` is the optional form. Internally `T?` is represented as a `Nullable[T]` wrapper but emits as `T | None` in Python annotations. The checker uses flow-sensitive analysis to narrow `T?` to `T` inside guards and null-checks. Attempting to call a method on a `T?` without a check is a compile error.

Narrowing forms the checker recognises:

- `is None` / `is not None`
- `isinstance(x, T)` (including tuple-of-types `isinstance(x, (A, B))`)
- `guard x = expr else: ...` early-return
- `if x is None: return` / `raise` / `break` / `continue` early-exit
- Exhaustive `match` arms — covers sealed unions, `Result`, and since
  v0.10.0 also `bool` subjects (a `case True:` / `case False:` pair),
  string-literal unions (`type C = "a" | "b"` with a guardless
  `case "literal":` per variant), and irrefutable fixed-arity tuple
  patterns (`match` on `tuple[int, int]` ending in `case (x, y):`)
- `body if test else orelse` ternary (v0.7.0) — narrows like `if`/`else`
- De Morgan refinement (v0.4.0) — `if not (A or B): return` narrows both operands afterwards
- `while` test-implied narrowings applied to the body (v0.3.0)
- **`assert x is not None`** (v0.9.0) — the standard Python static-checker idiom
- **Post-`while`-loop narrowing** (v0.9.0) — after `while y is None: y = load()` (no `break`), the post-loop `y` is narrowed to non-`None`
- **`while True:` reachability** (v0.9.0) — a body that always returns/raises on every branch with no `break` marks the post-loop point as unreachable, so `missing_return` doesn't fire on the surrounding function

### Augmented assignment is type-checked (v0.10.0)

`s += 5` on a `str` target now fires `tyc::operator_type_mismatch`, the
same as `s = s + 5`. The check is restricted to scalar targets
(`int` / `float` / `bool` / `str` / `bytes`); mutable containers keep
their looser in-place semantics (`list += any_iterable`) to avoid false
positives.

### Read-view covariance (v0.9.0)

For built-in containers the read-only protocols are covariant on their type parameter: `list[Subclass]` / `tuple[Subclass]` / `set[Subclass]` / `frozenset[Subclass]` flow into `Sequence[Super]` / `Iterable[Super]` / `Iterator[Super]` / `Collection[Super]` / `Container[Super]` / `Reversible[Super]` when `Subclass` inherits `Super`. `dict[K, V]` is K-invariant + V-covariant under `Mapping[K, V]` and `MutableMapping[K, V]`. The covariance is read-protocol-only — `list[Subclass]` does *not* flow into `list[Super]` (writes through the wide view would corrupt the underlying typing).

### Variant → parametric union assignability (v0.9.0)

Given `type LL[T] = Cons[T] | Nil`, a `Cons[T]` value is assignable into a `LL[T]` slot. Required for recursive ADT walks where the variant carries the same type parameter as the alias. Non-parametric variants (e.g. a plain `class Nil:` with no `[T]`) flow into `LL[T]` for any `T` because the variant has no `T`-dependent shape.

### Python-semantic alignment

The checker treats `or`/`and` the way CPython does: the expression yields one of the operands, not a `bool`. `let chunk: str = update.text or ""` therefore type-checks — the result type is `Union[truthy(typeof(lhs)), typeof(rhs)]` (and the falsy dual for `and`). Generator functions are structurally assignable to `Iterable[T]` / `Iterator[T]` / `AsyncIterable[T]` / `AsyncIterator[T]`, so `def compose() -> ComposeResult: yield ...` flows into a parameter annotated `Iterable[Widget]` without needing a manual list materialisation. Both checks fixed real-world adopter rejections — the broader audit is tracked under the `tyc::python_semantic_drift` warning.

### Generics

PEP 695 bracket syntax (`def f[T](x: T) -> T`, `type Vec[T] = list[T]`) — chosen at Phase 3 entry because `ruff_python_parser` already accepts it and it stays in lockstep with CPython grammar. Type parameters declare into the function/class scope as `Type::TypeVar(name)` and survive through signatures.

Inference is bidirectional: call sites bind typevars from actual arguments (recursively, e.g. `list[T]` against `list[int]` infers `T = int`; conflicting bindings widen to a union) and substitute them in the return type. Multi-argument constraint solving and bounded-type-var checking are wired up; full variance and higher-kinded forms remain partial. Generics are **type-erased** at emit time; runtime relies on Python's duck typing and (where present) Pydantic validation.

Type parameters on `impl[T]` blocks scope over the methods inside, and methods can introduce additional type parameters of their own:

```python
class Box[T]:
    value: T

impl[T] Box[T]:
    def map[U](self, f: Callable[[T], U]) -> Box[U]:
        return Box(value=f(self.value))
```

Both `T` (from the `impl` block) and `U` (from `map`) resolve inside the method body; call-site inference binds `U` from `f`'s return type.

Transparent `type` aliases — including generic ones (`type StringMap[V] = dict[str, V]`) and unions (`type B = int | str`) — are unwrapped during assignability checks, so `Ok[T]` flows into a `Result[Alias, E]` annotation and a literal that satisfies the underlying union flows into the alias.

### Interfaces (structural)

`interface` declarations are structural contracts, like Python's `typing.Protocol`. The checker verifies that a candidate type provides every required member with compatible signatures, with memoised "assumed subtype" sets to handle recursion. Interfaces emit as `typing.Protocol` subclasses.

Python's `@runtime_checkable` only validates **attribute presence** at runtime, not signatures. Typhon therefore does not lower `is`-tests or `isinstance` checks against an interface to a bare Python `isinstance` — a runtime conformance check requires an explicit opt-in keyword, otherwise the check fails to compile. Static structural typing remains the primary guarantee.

### Sealed unions

```
type Shape = Circle | Rectangle | Triangle
```

declares a finite, sealed sum type. `match` on a sealed union must cover every variant or include a wildcard. The single biggest static-safety win over current Python and mechanically simple to implement.

### Function signatures (Rule 1)

Every parameter and the return type carry an annotation. Functions returning nothing must spell `-> None`. Since v0.9.0 the rule also applies to `*args` / `**kwargs` — the canonical idiom for genuinely variadic functions is `*args: object` / `**kwargs: object`:

```python
def trace[R](f: Callable[..., R], *args: object, **kwargs: object) -> R:
    return f(*args, **kwargs)
```

`object` is the honest spelling for "really any value" at the boundary; the body still has to narrow with `isinstance` to do anything type-specific. Avoid `*args: Any` — `Any` silently drops every check.

### Explicit type instantiation is rejected (v0.9.0)

`func[T](args)` at the call site is rejected at check time with `tyc::operator_type_mismatch`. Type parameters are always inferred from arguments or the binding type. To pin a parameter, annotate the binding (forward) or the result position (backward):

```python
let p: tuple[int, int] = pair(1, 2)    # ✅ T inferred as int via the binding
let xs: list[int] = first([])          # ✅ T inferred via the binding
let xs = pair[int](1, 2)               # ❌ check-time error since v0.9.0
```

Before v0.9.0 the explicit form crashed at runtime with `'function' object is not subscriptable`. Generic *class* construction (`Box[int](value=7)`) is not affected — the `[int]` there is part of the class shape, not function-application syntax.

### No implicit `Any`

`Any` is a top type, but its inference is a compile error outside an explicit `unsafe` block. Untyped library calls must be wrapped in `unsafe` or shimmed with a `.dty` stub. Strictly stricter than TypeScript's `noImplicitAny`.

`unsafe` is a **lexical region**, not a per-value annotation. Inside it:

- Expressions that would otherwise infer to `Any` bind freely.
- Values acquire a hidden `Unsafe[T]` marker (visible in diagnostics, not in source).
- An `Unsafe[T]` cannot flow into a non-`unsafe` context expecting a concrete `T` — the user must re-assert the type via an annotated `let`/`mut`, a narrowing check, or an explicit cast.

Dynamic typing enters Typhon only through `unsafe` blocks and `.dty` stubs; nothing else.

## Classes and `impl` blocks

`class` declarations are minimalist: no explicit `__init__`, no body methods. Separate `impl` blocks attach methods to a class, Rust-style. Method definitions take an explicit `self` and reference fields as `self.NAME`; the desugarer merges them back into the class definition.

Default emit target is `@dataclass(slots=True)`. Pydantic emission is opt-in via the `model` keyword.

```python
# Typhon
class User:
    id: int
    name: str = "anon"
    email: str?

# Emitted Python (default: dataclass)
from dataclasses import dataclass

@dataclass(slots=True)
class User:
    id: int
    name: str = "anon"
    email: str | None = None
```

```python
# Typhon
model ApiUser:
    id: int
    email: str

# Emitted Python (model keyword: Pydantic)
from pydantic import BaseModel, ConfigDict

class ApiUser(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: int
    email: str
```

`model` emission injects `extra='forbid'` by default — Pydantic's stock `extra='ignore'` silently drops unexpected input, which directly contradicts Typhon's safety pitch. Permissive modes are opt-in via `[emit] model-extra = "allow" | "ignore"` in `typhon.toml`. Pydantic's `frozen=True` is *faux* immutability (it blocks field reassignment but does not freeze nested mutable values); see `let`/`mut` for what Typhon's binding immutability does and does not guarantee.

#### Mutable field defaults

`@dataclass` rejects bare mutable literals at class-definition time (`tags: list[str] = []` raises `ValueError` in Python). Typhon rewrites every `field: T = [] | {} | set() | list() | dict()` on a class that emits as a dataclass into `dataclasses.field(default_factory=<ctor>)` at desugar time, so the literal form just works:

```python
# Typhon
class Cart:
    items: list[str] = []
    seen: set[int] = set()

# Emitted Python
import dataclasses

@dataclasses.dataclass(slots=True)
class Cart:
    items: list[str] = dataclasses.field(default_factory=list)
    seen: set[int] = dataclasses.field(default_factory=set)
```

The rewrite is skipped for `model`, `interface`, and `class!` bodies, where the default's evaluation semantics differ (Pydantic validates, `__init__` is hand-synthesised, etc.).

#### `@property` accessors

`@property` on an `impl`-block method is recognised by the type checker: instance-level attribute access (`rect.area`, where `rect` is a `Rect` value — not `Rect.area` on the class object) resolves to the property's return type, not the underlying `() -> T` callable. `let area: float = rect.area` therefore type-checks without a parenthesised call.

```python
class Rect:
    w: float
    h: float

impl Rect:
    @property
    def area(self) -> float:
        return self.w * self.h
```

### `class!` (raw class)

`class!` is the escape hatch for classes that cannot be expressed as a dataclass: `torch.nn.Module`, `enum.Enum`, `typing.NamedTuple`, `unittest.TestCase`, Django models, SQLAlchemy declarative bases — anything whose base class needs a non-trivial `__init__` to run *before* fields are assigned.

```python
# Typhon
import torch.nn as nn

class! MyModel(nn.Module):
    layer: nn.Linear
    dropout: float

    def forward(self, x):
        return self.layer(x)

# Emitted Python (no @dataclass; super().__init__() runs first)
import torch.nn as nn

class MyModel(nn.Module):
    layer: nn.Linear
    dropout: float

    def __init__(self, layer: nn.Linear, dropout: float) -> None:
        super().__init__()
        self.layer = layer
        self.dropout = dropout

    def forward(self, x):
        return self.layer(x)
```

What `class!` changes versus a plain `class`:

- **No `@dataclass` decorator** is injected. The class is emitted verbatim with whatever bases it declares.
- **`__init__` is auto-synthesised** when the body declares no `def __init__` and at least one base is present. The synthesised constructor calls `super().__init__()` and then assigns every annotated field through `self`, in source order. Field defaults flow into the parameter signature; fields without defaults are positional, fields with defaults are keyword-or-positional after them.
- **Class-level field defaults are stripped from the body** when `__init__` is synthesised — the default is carried only in the generated parameter list. Leaving the literal at class scope would evaluate it twice (once at class-definition time as a shared class attribute, then again per-instance in `__init__`), which silently breaks libraries that introspect class attributes — e.g. PyTorch parameter registration would see a dead class-level `Linear(10, 5)` instance. Annotations survive so type checkers still see the field shape.
- **A hand-written `__init__` is preserved verbatim.** Use this when the base class needs configuration arguments that aren't 1:1 with your declared fields.

### `plain class` (raw Python class)

`plain class X:` is the small-step escape hatch for "I want a plain Python
class — no decorator, no synthesised constructor, no slots." It is the
counterpart to `class X frozen:` and reads at a glance: anyone scanning
the file sees that this type follows Python's stock semantics, not
Typhon's dataclass-by-default rule.

```python
# Typhon
plain class Bag:
    items: list[str]
    label: str = "unsorted"

# Emitted Python (no decorator at all)
class Bag:
    items: list[str]
    label: str = "unsorted"
```

`plain class` differs from `class!` in one key way: it does **not**
synthesise an `__init__`. The body emits verbatim, so attributes only
exist on instances once user code assigns them — exactly like a hand-
written Python class. Reach for `plain class` when you want Python's
permissive semantics for metaclass-driven libraries (Textual, Django ORM
descriptors, SQLAlchemy declarative models) that set instance attributes
dynamically. Reach for `class!` when you're subclassing a framework base
that needs `super().__init__()` to fire before fields are wired up.

Two related auto-skip rules tighten the safety net:

- **Auto-skip on framework-base inheritance.** A plain `class Foo(Base):`
  whose base is one of `Enum` / `IntEnum` / `StrEnum` / `Flag` /
  `IntFlag` / `ABC` / `ABCMeta` is emitted without `@dataclass`, so
  enum subclasses and abstract bases work without needing the `class!`
  or `plain class` marker. `Protocol`, `TypedDict`, and `NamedTuple`
  subclasses have always been skipped. Project-specific framework bases
  can be added via `[emit] skip-decoration-bases` in `typhon.toml` —
  matched by last identifier segment so `"App"` catches both
  `class T(App):` and `class T(textual.App):`. Auto-skip drops only the
  decorator; it does not synthesise an `__init__`. Use `class!` when
  you need both the dropped decorator *and* a generated constructor
  that calls `super().__init__()`.
- **`class-default` validation.** `class-default = "struct"` /
  `"regular"` / `"none"` used to be silently identical to `"dataclass"`.
  They are now rejected at config load with
  `tyc::invalid_config_value` and the allowed-values list
  (`"dataclass"`, `"pydantic"`).

When to reach for which class form:

| Form | Emits | Use when |
|---|---|---|
| `class Foo:` | `@dataclass(slots=True)` | Plain value type. Default for new code. |
| `class Foo frozen:` | `@dataclass(slots=True, frozen=True)` | Immutable value type — field reassignment is a hard error. |
| `model Foo:` | `BaseModel` (Pydantic, `extra='forbid'`) | Validated input at a system boundary. |
| `interface Foo:` | `Protocol` | Structural contract you check against, not a concrete type. |
| `plain class Foo:` | bare `class Foo:` (no decorator, no synthesised `__init__`) | Metaclass-driven libraries, descriptor-based models, anything that owns its own attribute layout. |
| `class! Foo(Base):` | bare `class Foo(Base):` + synthesised or hand-written `__init__` | Subclassing a framework base that owns its own `__init__`. |
| `enum Foo:` (v0.11.0) | `class Foo(enum.Enum):` with `enum.auto()` for bare members | A fixed set of named members, sugar over `enum.Enum`. |

### `enum` keyword (v0.11.0)

`enum Name:` declares an `enum.Enum` subclass. Bare members
auto-number via `enum.auto()`; explicit `MEMBER = value` is preserved
and `enum.auto()` continues numbering from the last explicit value.
`tyc-syntax` preprocesses the header / body, `tyc-emit` injects
`import enum`, and `tyc-resolve` adds `enum` to the builtin prelude so
the form type-checks before the import is rewritten in. `tyc fmt`
round-trips the header and members.

```typhon
enum Shape:
    CIRCLE
    SQUARE
    TRIANGLE

enum Color:
    RED = 1
    GREEN = 2
    BLUE = 4
```

Emits:

```python
import enum

class Shape(enum.Enum):
    CIRCLE = enum.auto()
    SQUARE = enum.auto()
    TRIANGLE = enum.auto()

class Color(enum.Enum):
    RED = 1
    GREEN = 2
    BLUE = 4
```

Under `tyc run` the native `enum` module materialises members on first
class-body execution, iteration is in declaration order, and
`Shape.CIRCLE` repr matches CPython (`<Shape.CIRCLE: 1>`).

## Error handling

### `Result[T, E]`

`Result[T, E]` is a sealed sum type with two constructors, `Ok(T)` and `Err(E)`. Emits as a tagged dataclass in a generated `typhon_runtime/` module — no PyPI dependency.

### The `?` operator

`?` suffix on a `Result`-typed expression unwraps `Ok` and short-circuits `Err` to the enclosing function. The checker enforces that `?` appears only inside a function whose return type is a compatible `Result`. Desugaring is a localised `if isinstance(_x, Err): return _x; v = _x.value` pattern — not try/except, to keep stack traces clean.

**Comprehension carve-out:** `?` is also rejected inside list / set / dict comprehensions and generator expressions (`tyc::invalid_question_op`). Comprehensions lower to nested `for`-loops, so the surrounding function frame `?` would short-circuit out of is not the comprehension's frame. Since v0.9.0 the diagnostic help text mentions both causes explicitly. Pre-extract with an explicit loop or chain `.and_then` / `.map` instead.

### `Result` combinators

`Ok` and `Err` expose `.map(f)`, `.map_err(g)`, `.and_then(f)` (chain a `Result`-returning op), and `.or_else(h)` (recover from an `Err`) as methods on the runtime classes. They landed in v0.6.0 for the compiled path; since v0.9.0 they also work under `tyc run` (the VM binds them as native methods via `NativeFn` wrappers that capture the receiver).

### `with`-chains

Modelled on Elixir: sequences `Result`-producing expressions, binding the success value of each, with a single `else` block that catches the first `Err`.

```python
# Typhon
with user   = db.find_user(id)?,
     perms  = check_perms(user)?,
     report = build_report(user, perms)?:
    return Ok(report)
else err:
    log.warn(err)
    return Err(err)
```

Since v0.8.0 the implicit propagation path on every `?`-binding is type-checked against the enclosing function's declared error type — a chain that routes a mismatching error class no longer slips through. Since v0.9.0 the same check covers the explicit `else err: return Err(err)` form too — previously the check was gated on the synthetic `?`-op temp shape, so an `else` block could silently return an `Err` whose payload type didn't match the function's declared return.

### Bridging exceptions: `try_result` and postfix `rescue`

Third-party Python raises exceptions. Rather than a `try/except` shim, lift the boundary into a `Result` and use `?` / `match` downstream. The `try_result(thunk[, on_err])` combinator runs `thunk()`, returning `Ok(result)` or, on any exception, `Err(on_err(exc))` (or `Err(exc)` when no mapper is given).

For the common single-boundary case, the **postfix `rescue` operator** says the same thing with no lambdas, no `try`, and no `except`:

```python
def load_port(raw: str) -> Result[int, str]:
    let n: int = int(raw) rescue e: f"bad port: {e}"     # ✅ lambda-free boundary
    return Ok(n)
```

`EXPR rescue NAME: ERR_EXPR` runs `EXPR`; on any exception it binds the exception to `NAME`, evaluates `ERR_EXPR`, and propagates `Err(ERR_EXPR)` to the enclosing function exactly like `?` (so the enclosing function must return a compatible `Result`). It is surface sugar for `try_result(lambda: EXPR, lambda NAME: ERR_EXPR)?`, and composes with `as!`:

```python
let data: dict[str, str] = json.loads(text) as! dict[str, str] rescue e: f"bad json: {e}"
```

A **block form** maps any exception raised across a whole suite — the lambda-free replacement for a `try/except` shim:

```python
def load_config(text: str) -> Result[Config, str]:
    rescue e: f"bad config: {e}":
        let data: dict[str, str] = json.loads(text) as! dict[str, str]
        return Ok(Config(host=data["host"], port=int(data["port"])))
```

The block form lowers to `try` / `except Exception as e: return Err(...)`. The mapped error type is checked against the function's declared error type in both forms (`tyc::result_error_mismatch`). `rescue` works in value positions and after a leading `return` / `if` / `while` / `assert`. **Scope (v1):** postfix `rescue` is lowered in statement-tail position (the last thing on the line); an inline/mid-expression postfix `rescue`, or one whose right side isn't `NAME: EXPR`, is left for the parser.

## Async and concurrency

### Explicit `async`, not inferred

Async-by-default with full inference is rejected. Inferring async means a function's "colour" changes when a deep callee changes, which makes refactoring fragile and stack traces confusing.

Typhon does add static checks:
- A function declared `async` that contains no `await` is a warning.
- A sync function that calls an async one without `await` is a hard error.

### Automatic `asyncio.gather` (opt-in)

The analyser rewrites sequences of independent `await` statements as `asyncio.gather`, but only when (a) every called function is `@pure`, (b) LHS bindings do not alias, and (c) the statements form a straight-line block. A more aggressive mode is opt-in via the explicit `gather` keyword:

```python
# Typhon: explicit, always safe
gather:
    user   = fetch_user(id)
    posts  = fetch_posts(id)
    notifs = fetch_notifications(id)

# Emitted Python (default: TaskGroup — cancels siblings on first failure)
async with asyncio.TaskGroup() as _tg:
    _t_user   = _tg.create_task(fetch_user(id))
    _t_posts  = _tg.create_task(fetch_posts(id))
    _t_notifs = _tg.create_task(fetch_notifications(id))
user   = _t_user.result()
posts  = _t_posts.result()
notifs = _t_notifs.result()
```

`gather:` lowers to `asyncio.TaskGroup` (3.11+) by default. `asyncio.gather(...)` propagates the first exception but lets siblings keep running, which is the wrong default for side-effectful work. Users who genuinely want partial-success semantics opt in via `gather(strategy="best-effort"):`, which lowers to `asyncio.gather(..., return_exceptions=True)`.

### Free-threaded Python

When `typhon.toml` sets `[python] free-threaded = true`, Typhon targets a free-threaded CPython build (3.13t / 3.14t / 3.15t) and unlocks two opt-in build-time rewrites plus two advice lints. Default-off until 3.14 is the default Python.

**Auto-parallel comprehensions** (`[strictness] auto-parallel = true`). A list / set / dict comprehension whose element is a *pure call* is rewritten to `typhon_runtime.parallel.map_pure(lambda x: <elt>, <source>)`. The eligible shapes are:

- `[f(x) for x in xs]` — the baseline pure-call element;
- `[f(x) for x in xs if COND]` — a **filter** whose `COND` is itself pure (loop target, literals, arithmetic / comparison / boolean operators, pure calls) runs sequentially in the map's source list, the pure element map runs in parallel;
- `[f(x, k) for x in xs]` — **extra arguments** that are literals or `let`-bound loop invariants (a `mut`-bound name is never captured);
- `[g(f(x)) for x in xs]` — **nested** pure calls.

Every widening is semantics-preserving because the element, its captured arguments, and the filters are all side-effect-free. `[strictness] parallel-min-size` (default 64) suppresses the rewrite for statically-sized literal iterables below the threshold.

**Auto-parallel integer reductions** (`[strictness] auto-parallel-reductions = true`, which also requires `auto-parallel`). A canonical accumulation loop `for x in ITER: total += EXPR` — with a **plain `int`** accumulator (`mut total: int`), a pure `EXPR`, and an invariant `ITER` — folds into `total += sum(typhon_runtime.parallel.map_pure(lambda x: EXPR, ITER))`. Integers only: integer addition is associative/commutative and Python ints are exact, so summing partial results in any order is identical. **Floats are never eligible** — reordering IEEE-754 addition changes the result. `ITER` must also be **provably bounded and effect-free to materialise** — a `list`/`tuple`/`set` literal, a bare name annotated `list[...]` / `tuple[...]` / `set[...]` / `frozenset[...]` in the loop's scope, or a direct builtin `range(...)` call (parallelising a `range` loop materialises it — an inherent cost of the map-based design) — because `map_pure` runs `list(ITER)` before evaluating any element; a call result, unannotated name, or generator never rewrites, since an unbounded iterator would hang where the sequential loop raises on its first element, and a stateful iterator's side effects would all run where the loop stopped early.

**Execution backend** (`[strictness] parallel-backend`, default `"threads"`). `map_pure` runs on a `ThreadPoolExecutor` (order-preserving; escapes the GIL on a free-threaded build, serialises but stays correct on a stock GIL build). Setting `parallel-backend = "interpreters"` first tries a PEP 734 `concurrent.futures.InterpreterPoolExecutor` (Python 3.14+) and falls back **transparently** to the thread pool on `ImportError` / `AttributeError` (older runtimes) or when the mapped function can't be pickled across the interpreter boundary — probed with `pickle.dumps` before any pool is created, so an unshareable callable falls back whatever exception pickling raises, while exceptions raised *by* the mapped function still propagate normally. Order is preserved on every path. The lambdas the auto-parallel rewrites emit never pickle, so rewritten call sites always run on the thread pool under this backend today; the interpreters pool benefits hand-written `map_pure` calls passing top-level named functions.

**Advice lints** (both gated by `[strictness] suggest-parallel`, default on, and both silent unless `free-threaded = true`):

- [`tyc::parallel_opportunity`](diagnostics/parallel_opportunity.md) nudges a comprehension or integer accumulator loop that *would* be rewritten if the relevant knob were on — or a `float` accumulator loop that matches every reduction condition except the required `int` annotation (parallelisable only by reordering float addition, which changes results).
- [`tyc::shared_mut_across_tasks`](diagnostics/shared_mut_across_tasks.md) flags a `go`-spawned same-module function that writes module-level mutable state (a `global` assignment or a write to a module-level `mut` binding), since under free-threaded Python the spawned task runs concurrently with the spawner.

### `go` spawn

`go f(x)` schedules `f(x)` in the background: an `asyncio.Task` in async contexts, a `ThreadPoolExecutor.submit` future on free-threaded builds for CPU-bound functions. `go f(x) -> fut` binds the task handle.

`go` lowers through `typhon_runtime.tasks.spawn`, **never** to a bare `asyncio.create_task`. Python's event loop holds only weak references to tasks, so a fire-and-forget task whose handle is dropped can be garbage-collected mid-flight. The runtime helper keeps a strong-ref registry and discards entries from a done-callback. Same pattern, different registry, for thread-pool `go` on free-threaded builds.

## `let` and `mut`

`let` and `mut` govern **binding immutability**, not deep value immutability — like Rust's `let`/`let mut` or TypeScript's `const`. A `let u: User` cannot be reassigned, but a mutable field on `u` can still be written through.

- `let` is immutable as a binding. Reassignment is a compile error.
- `mut` is mutable. Parallelisation passes refuse to touch any binding captured as `mut` by a spawned task without explicit synchronisation.
- Top-level module bindings default to `let` unless declared `mut`.
- Inside a function, every local binding must declare `let` or `mut` on first occurrence (`tyc::missing_binding_kind` otherwise). Three carve-outs:
  - Names declared `global` or `nonlocal` inside the same function refer to an outer-scope binding whose `let`/`mut` already lives at the declaration site, so the bareword assignment is accepted.
  - Bindings introduced by the walrus operator (`if (n := len(xs)) > 3:`) don't require a leading keyword; they are implicitly immutable (`let`-equivalent) and cannot be rebound without `mut`.
  - Bindings introduced inside a `gather:` block (the keyword itself declares them as immutable single-assignment names) don't take `let`/`mut`.

```python
mut counter: int = 0

def inc() -> None:
    global counter
    counter = counter + 1   # OK — `counter` is declared at module scope
```

Deep immutability for class instances is an emit-time concern: pass `frozen=True` to the underlying dataclass / Pydantic config. A `freeze` modifier with stronger recursive guarantees may land later; `let` itself stays scoped to bindings.

### Tuple-unpacking `let` (with per-element types)

`let (a: int, b: str) = func(x, y)` is sugar for "bind the result of `func(x, y)` to a hidden temp, then introduce `a: int` and `b: str` from the temp's first and second elements." Compound annotations survive the top-level-comma split (`let (xs: list[int], m: dict[str, int]) = …`), and mixed forms emit the un-annotated leg without a type so the checker fills it in (`let (a: int, b) = pair()`). The un-annotated tuple form (`let (a, b) = pair()`) continues to flow through unchanged with no synthetic temp.

### Declare-only `let NAME: T` with arm assignment

A `let NAME: T` declaration without an initialiser is legal when the binding is initialised on every non-diverging branch of a subsequent `if`/`elif`/`else` or `match` block. The first assignment on each path is treated as the initialiser; the resolver tracks each uninitialised `let`-declaration's span and silently accepts the first assignment, while any second assignment (or any read on a path that didn't assign) fires `tyc::immutable_assign` or `tyc::use_of_uninitialised` respectively.

```ty
let loaded: Cfg
match _load():
    case Ok(v):
        loaded = v
    case Err(e):
        return Err(e)
loaded.use()        # OK — every non-diverging arm assigned `loaded`
```

`match` over a sealed union or `Result[T, E]` is treated as exhaustive for definite-assignment purposes when every variant is covered by a class pattern; the canonical `case Ok(v): / case Err(e): return Err(e)` shape works without a `case _:` wildcard. `return` / `raise` / `continue` / `break` mark a branch as diverging — the branch is excluded from the intersection of "definitely-assigned" paths. Loop bodies do not propagate assignments out (the body may execute zero times).

`mut NAME: T` without an initialiser is also accepted and follows the usual `mut` semantics — any number of subsequent assignments are legal.

## Lazy loading

- `lazy import np = numpy` → defers module loading until first attribute access. On a **3.13 / 3.14 target** this lowers to a call to the generated `typhon_runtime.lazy.lazy_import` helper (which wraps the stdlib `importlib.util.LazyLoader`). On a **3.15+ target** it lowers to native [PEP 810](https://peps.python.org/pep-0810/) syntax instead — see below.
- `lazy from foo import a, b` is **rejected** at parse time: PEP 690 notes that `from`-imports eagerly touch attributes on the source module and therefore defeat deferral. Use `lazy import foo` and access `foo.a` / `foo.b`. (Note: PEP 810 permits a `lazy from … import` form upstream, but Typhon keeps the single `lazy import ALIAS = MODULE` surface for now — supporting the `from` form is a future surface decision.)
- `lazy let` module-level bindings → cached getter with a sentinel + lock helper in `typhon_runtime` (not `functools.cached_property`, which is instance-scoped, race-prone, and writable after first evaluation).
- `lazy let` instance-level bindings on effectively immutable classes → `functools.cached_property`.
- `lazy[list[T]]` return types → generator functions instead of materialised lists.

### Native lazy imports on Python 3.15 (PEP 810)

Python 3.15 ships [PEP 810](https://peps.python.org/pep-0810/): a native `lazy import` statement with exactly the deferred-until-first-use semantics Typhon's helper emulates. When a project targets `3.15` / `3.15t`, `tyc build` lowers `lazy import` directly to that native form — no `typhon_runtime` helper, no runtime import:

```python
# Typhon source (any target)
lazy import np = numpy

# Emitted Python — 3.13 / 3.14 target
from typhon_runtime.lazy import lazy_import as __typhon_lazy_import
np = __typhon_lazy_import("numpy")

# Emitted Python — 3.15+ target
lazy import numpy as np
```

A project whose only runtime-touching feature was `lazy import` therefore ships **no** generated `typhon_runtime/` package on a 3.15+ target. The change is `tyc build`-only and only on 3.15+ — 3.13 / 3.14 output is byte-for-byte unchanged, and `tyc check` / `tyc run` are unaffected. (If `[checker] external = "ty"` is enabled, run it with a PEP 810-aware `ty`; an older `ty` build will reject the native `lazy import` syntax in the emitted Python.)

## Stubs and Python interop

Typhon authors `.dty` stubs; the compiler emits standard PEP 561 `.pyi` for interop. Both formats coexist by design:

- **`.dty`** is the Typhon source of truth and keeps the stricter dialect (`T?`, `Result[T, E]`, sealed unions, interfaces, `unsafe`).
- **`.pyi`** is the interop artefact every other Python tool already understands (mypy, pyright, Pyrefly, ty, IDEs).

The emitter lowers Typhon-only forms back to typing-spec equivalents (`T?` → `T | None`, sealed unions → `Union[...]` with `Literal` tags where appropriate, `Result[T, E]` → the runtime-helper classes by their generated import path). A `.pyi` consumed *by* Typhon is treated as an `unsafe` boundary unless an authored `.dty` overrides it.

Drift between a `.dty` and the runtime module it describes is caught by `tyc check --stubs`, an in-tree port of mypy's `stubtest`: it compares the compiled `.pyi` against the runtime symbols of the implementation module and reports missing names, signature drift, and constructor arity mismatches.

## Purity inference and memoisation

A function is **inferable as pure** only if every one of the following holds:

1. Synchronous. Coroutines, generators, and async generators are excluded.
2. All parameter types are hashable (primitives, frozen dataclasses, tuples of hashable types, sealed-union variants whose payloads are hashable).
3. No I/O in the transitive call graph: no `open`, `socket`, `subprocess`, logger writes, `print`, DB drivers. `unsafe` and stubbed calls count as impure unless the stub is annotated `@pure`.
4. No reads from non-deterministic clocks or entropy sources (`time.time`, `time.monotonic`, `random.*`, `secrets.*`, `uuid.uuid4`, `os.urandom`).
5. No reads from or writes to mutable module-level state. Reads from `comptime let` bindings are fine; reads from a `mut` module binding are not.
6. No exceptions raised — pure functions express failure through `Result[T, E]`.

When all six hold, the analyser **may** emit `@functools.cache` or `@functools.lru_cache(maxsize=N)` — but only with an explicit opt-in: a `@memo` attribute on the function, an `@pure(memo=True)` annotation, or `[strictness] auto-memoise = true` in `typhon.toml`. The checker never inserts caches silently; caches extend the lifetime of every argument and return value, which is not a transparent change.

Manually marking a function `@pure` that fails any of the six conditions is a hard error.

## Compile-time evaluation (`comptime`)

`comptime` bindings are evaluated by `tyc` at compile time in a sandboxed interpreter that supports pure arithmetic, string operations, environment-variable lookup via `env(name, default?)`, simple container construction, and calls to other `comptime` functions. Results are inlined as literals.

Build-time env validation alone is worth shipping.

```python
# Typhon
comptime let PORT: int = int(env("PORT", "8080"))
comptime let DB_URL: str = env("DATABASE_URL")  # build fails if unset

# Emitted Python
PORT: int = 8080
DB_URL: str = "postgresql://..."
```

### `comptime def` functions

A `comptime def` declares a function that the evaluator can call from a `comptime let` initialiser. The function name is registered at build time; the body can mix `return` with local bindings (`x = EXPR`, `let x: T = EXPR`, `mut x: T = EXPR`) and `if` / `elif` / `else` branches. Expressions follow the same rules as any other comptime RHS — literals, arithmetic, string concatenation, comparisons, boolean ops, ternaries, `env()` / `int()` / `str()` / `float()` calls, parameter references, and calls to other `comptime def` functions. The full statement and expression grammar is enumerated in the "v1 contract" section below.

```python
# Typhon
comptime def double(n: int) -> int:
    return n * 2

comptime def join(prefix: str, suffix: str) -> str:
    return prefix + suffix

comptime let PORT:    int = double(4000)
comptime let API_URL: str = join("https://api.", env("DOMAIN", "example.com"))
```

The Typhon checker invokes `double` and `join` at compile time and substitutes the resulting literals before emission:

```python
# Emitted Python
def double(n: int) -> int:
    return n * 2

def join(prefix: str, suffix: str) -> str:
    return prefix + suffix

PORT: int = 8000
API_URL: str = "https://api.example.com"
```

The function definitions remain in the emitted output (they're ordinary Python `def`s — the `comptime` prefix is a build-time marker, not a runtime signal) so the same helpers stay available at runtime should you also call them from non-comptime code.

The contract is intentionally tight in v1, but already covers most build-time configuration shapes:

- **Statements**: `return EXPR`, local bindings (`x = EXPR`, `let x: T = EXPR`, `mut x: T = EXPR`), and `if`/`elif`/`else` are supported. Loops, exceptions, `with`-blocks, `class`/`def` declarations, and `raise` are not — call sites should compose smaller comptime helpers instead.
- **Expressions**: every form available to a `comptime let` initialiser, plus parameter and local-binding references, comparisons (`==`, `!=`, `<`, `<=`, `>`, `>=`), boolean operators (`and`, `or`, `not`), and the `EXPR if COND else EXPR` ternary.
- **Parameters** must be plain positional names — no defaults, `*args`, `**kwargs`, or keyword-only forms.
- **Free variables** (module-level names other than parameters and local bindings) are not in scope inside the body. Comptime evaluation is hermetic — call sites pass everything in as arguments.
- **Recursion depth** is capped (currently 64) so a buggy definition fails the build rather than hanging it.

These restrictions exist because comptime evaluation runs *inside the compiler*. Lifting them further (loops, container construction, types as values) is incremental work; the rule of thumb today is "if a comptime function couldn't be a small pure helper over arithmetic, strings, and booleans, it probably belongs at runtime."

Concrete examples that work today:

```python
comptime def grade(score: int) -> str:
    if score >= 90:
        return "A"
    elif score >= 80:
        return "B"
    elif score >= 70:
        return "C"
    else:
        return "F"

comptime def clamp_port(p: int) -> int:
    let lower: int = 1024
    let upper: int = 65535
    if p < lower:
        return lower
    if p > upper:
        return upper
    return p

comptime let MY_GRADE:  str = grade(82)           # → "B"
comptime let SAFE_PORT: int = clamp_port(80)      # → 1024
comptime let MAX_SCORE: int = 100 if env("STRICT", "1") == "1" else 80
```

## Readability features

### Guards

`guard x = expr else: ...` is sugar for assignment plus an early-return on the falsy/None case. The checker narrows `x` to non-null after the guard.

### Pipe operator

`a |> f() |> g(arg)` desugars to `g(f(a), arg)`. Left-associative; the pipe argument fills the first positional slot of the next call.

### Extension methods

`extend ClassName:` attaches methods to a user-defined class declared elsewhere — `impl`'s twin for code you don't want to keep in the original module. The merge happens at desugar; downstream callers see a single class with both sets of methods.

```
# domain/user.ty
class User:
    id: int
    name: str

# analytics/user_metrics.ty
extend User:
    def tracking_id() -> str:
        return f"user-{id:08d}"
```

`extend BUILTIN:` (extending the recognised Python built-ins — `str`, `list`, `int`, `dict`, …) is also supported. Each method is extracted at desugar time to a module-level free function `__typhon_ext_<TYPE>__<METHOD>`, and call sites `x.method(...)` are rewritten to `__typhon_ext_<TYPE>__method(x, ...)` whenever the receiver `x` has a static annotation matching one of the registered built-ins. There is no monkey-patching of built-in types; the rewrite is strictly opt-in by type annotation, so calls on un-annotated receivers continue to raise `AttributeError` at runtime, matching Python's existing semantics.
