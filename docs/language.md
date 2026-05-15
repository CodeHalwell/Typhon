# Language Design

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Each section covers what the feature does, how it desugars to Python, and what the checker must enforce.

## Type system

### Non-nullable by default

A plain `T` forbids `None`; `T?` is the optional form. Internally `T?` is represented as a `Nullable[T]` wrapper but emits as `T | None` in Python annotations. The checker uses flow-sensitive analysis to narrow `T?` to `T` inside guards and null-checks. Attempting to call a method on a `T?` without a check is a compile error.

### Generics

Angle-bracket syntax (`def f<T>(x: T) -> T`) instead of PEP 484 TypeVars. Inference is bidirectional: constraints flow from arguments to type parameters, falling back to explicit annotation when ambiguous. Generics are **type-erased** at emit time; runtime relies on Python's duck typing and (where present) Pydantic validation.

### Interfaces (structural)

`interface` declarations are structural contracts, like Python's `typing.Protocol`. The checker verifies that a candidate type provides every required member with compatible signatures, with memoised "assumed subtype" sets to handle recursion. Interfaces emit as `typing.Protocol` subclasses.

### Sealed unions

```
type Shape = Circle | Rectangle | Triangle
```

declares a finite, sealed sum type. `match` on a sealed union must cover every variant or include a wildcard. The single biggest static-safety win over current Python and mechanically simple to implement.

### No implicit `Any`

`Any` is a top type, but its inference is a compile error outside an explicit `unsafe` block. Untyped library calls must be wrapped in `unsafe` or shimmed with a `.dtt` stub. Strictly stricter than TypeScript's `noImplicitAny`.

## Classes and `impl` blocks

`class` declarations are minimalist: no explicit `__init__`, no `self` parameter on methods. Separate `impl` blocks attach methods to a class, Rust-style. The desugarer merges `impl` blocks into the class definition and inserts `self`.

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
from pydantic import BaseModel

class ApiUser(BaseModel):
    id: int
    email: str
```

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

# Emitted Python
user, posts, notifs = await asyncio.gather(
    fetch_user(id),
    fetch_posts(id),
    fetch_notifications(id),
)
```

### Free-threaded Python

When `typhon.toml` sets `free-threaded = true`, the analyser emits `ThreadPoolExecutor`-based parallelism for pure-function comprehensions on large collections. The emitter inserts a runtime `sys._is_gil_enabled()` check and falls back to sequential execution if the GIL is present. Default-off until 3.14 is the default Python.

### `go` spawn

`go f(x)` is sugar for `asyncio.create_task(f(x))` in async contexts and `concurrent.futures.ThreadPoolExecutor.submit` on free-threaded builds for CPU-bound functions. `go f(x) -> fut` binds the task handle.

## `val` and `var`

- `val` is immutable. Reassignment is a compile error.
- `var` is mutable. Parallelisation passes refuse to touch any binding captured as `var` by a spawned task without explicit synchronisation.
- Top-level module bindings default to `val` unless declared `var`.

## Lazy loading

- `lazy import np = numpy` → defers module loading until first attribute access via `importlib.util.LazyLoader`.
- `lazy val` module-level bindings → cached getter.
- `lazy[list[T]]` return types → generator functions instead of materialised lists.

## Compile-time evaluation (`comptime`)

`comptime` bindings are evaluated by `ttc` at compile time in a sandboxed interpreter that supports pure arithmetic, string operations, environment-variable lookup via `env(name, default?)`, simple container construction, and calls to other `comptime` functions. Results are inlined as literals.

Build-time env validation alone is worth shipping.

```python
# Typhon
comptime val PORT: int = int(env("PORT", "8080"))
comptime val DB_URL: str = env("DATABASE_URL")  # build fails if unset

# Emitted Python
PORT: int = 8080
DB_URL: str = "postgresql://..."
```

## Readability features

### Guards

`guard x = expr else: ...` is sugar for assignment plus an early-return on the falsy/None case. The checker narrows `x` to non-null after the guard.

### Pipe operator

`a |> f() |> g(arg)` desugars to `g(f(a), arg)`. Left-associative; the pipe argument fills the first positional slot of the next call.

### Extension methods

```
extend str:
    def to_slug() -> str: ...
```

emits a free function `str_to_slug(self: str) -> str` and rewrites call sites `"hello".to_slug()` as `str_to_slug("hello")` at emit time. No monkey-patching of built-ins.
