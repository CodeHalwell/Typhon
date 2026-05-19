# Language Design

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Each section covers what the feature does, how it desugars to Python, and what the checker must enforce.

## Type system

### Non-nullable by default

A plain `T` forbids `None`; `T?` is the optional form. Internally `T?` is represented as a `Nullable[T]` wrapper but emits as `T | None` in Python annotations. The checker uses flow-sensitive analysis to narrow `T?` to `T` inside guards and null-checks. Attempting to call a method on a `T?` without a check is a compile error.

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

When to reach for which class form:

| Form | Emits | Use when |
|---|---|---|
| `class Foo:` | `@dataclass(slots=True)` | Plain value type. Default for new code. |
| `model Foo:` | `BaseModel` (Pydantic, `extra='forbid'`) | Validated input at a system boundary. |
| `interface Foo:` | `Protocol` | Structural contract you check against, not a concrete type. |
| `class! Foo(Base):` | bare `class Foo(Base):` + synthesised or hand-written `__init__` | Subclassing a framework base that owns its own `__init__`. |

## Error handling

### `Result[T, E]`

`Result[T, E]` is a sealed sum type with two constructors, `Ok(T)` and `Err(E)`. Emits as a tagged dataclass in a generated `typhon_runtime/` module — no PyPI dependency.

### The `?` operator

`?` suffix on a `Result`-typed expression unwraps `Ok` and short-circuits `Err` to the enclosing function. The checker enforces that `?` appears only inside a function whose return type is a compatible `Result`. Desugaring is a localised `if isinstance(_x, Err): return _x; v = _x.value` pattern — not try/except, to keep stack traces clean.

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

When `typhon.toml` sets `free-threaded = true`, the analyser emits `ThreadPoolExecutor`-based parallelism for pure-function comprehensions on large collections. The emitter inserts a runtime `sys._is_gil_enabled()` check and falls back to sequential execution if the GIL is present. Default-off until 3.14 is the default Python.

### `go` spawn

`go f(x)` schedules `f(x)` in the background: an `asyncio.Task` in async contexts, a `ThreadPoolExecutor.submit` future on free-threaded builds for CPU-bound functions. `go f(x) -> fut` binds the task handle.

`go` lowers through `typhon_runtime.tasks.spawn`, **never** to a bare `asyncio.create_task`. Python's event loop holds only weak references to tasks, so a fire-and-forget task whose handle is dropped can be garbage-collected mid-flight. The runtime helper keeps a strong-ref registry and discards entries from a done-callback. Same pattern, different registry, for thread-pool `go` on free-threaded builds.

## `let` and `mut`

`let` and `mut` govern **binding immutability**, not deep value immutability — like Rust's `let`/`let mut` or TypeScript's `const`. A `let u: User` cannot be reassigned, but a mutable field on `u` can still be written through.

- `let` is immutable as a binding. Reassignment is a compile error.
- `mut` is mutable. Parallelisation passes refuse to touch any binding captured as `mut` by a spawned task without explicit synchronisation.
- Top-level module bindings default to `let` unless declared `mut`.
- Inside a function, every local binding must declare `let` or `mut` on first occurrence (`tyc::missing_binding_kind` otherwise). The one carve-out is for names declared `global` or `nonlocal` inside the same function: those refer to an outer-scope binding whose `let`/`mut` already lives at the declaration site, so the bareword assignment is accepted.

```python
mut counter: int = 0

def inc() -> None:
    global counter
    counter = counter + 1   # OK — `counter` is declared at module scope
```

Deep immutability for class instances is an emit-time concern: pass `frozen=True` to the underlying dataclass / Pydantic config. A `freeze` modifier with stronger recursive guarantees may land later; `let` itself stays scoped to bindings.

## Lazy loading

- `lazy import np = numpy` → defers module loading until first attribute access via a generated `__TyphonLazy_<alias>_` proxy class (thread-safe, double-checked locking).
- `lazy from foo import a, b` is **rejected** at parse time: PEP 690 notes that `from`-imports eagerly touch attributes on the source module and therefore defeat deferral. Use `lazy import foo` and access `foo.a` / `foo.b`.
- `lazy let` module-level bindings → cached getter with a sentinel + lock helper in `typhon_runtime` (not `functools.cached_property`, which is instance-scoped, race-prone, and writable after first evaluation).
- `lazy let` instance-level bindings on effectively immutable classes → `functools.cached_property`.
- `lazy[list[T]]` return types → generator functions instead of materialised lists.

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
