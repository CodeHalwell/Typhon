# Typhon — Syntactic Forms Reference

Every Typhon-specific form, listed side-by-side with the Python it lowers to. For background and design rationale, see `docs/language.md` and `docs/long-term-plan.md`.

The convention throughout: **Typhon source on the left or above; emitted Python on the right or below.** Where formatting matters, code is shown verbatim from the printer.

---

## Bindings

### Local bindings

```python
# Typhon
def demo() -> None:
    let pi: float = 3.14159
    mut count: int = 0
    count = count + 1
```

```python
# Emitted Python
def demo() -> None:
    pi: float = 3.14159
    count: int = 0
    count = count + 1
```

The `let`/`mut` keyword is enforced at compile time and erased at emit. Reassignment of a `let` is `tyc::immutable_assign`.

### Module-level bindings

```python
# Typhon
PI: float = 3.14159           # implicitly let
mut FEATURE_FLAG: bool = False
```

```python
# Emitted Python
PI: float = 3.14159
FEATURE_FLAG: bool = False
```

Inside a function, the kind is always explicit. At module top level, it defaults to `let` unless declared `mut`.

---

## Optional types

```python
# Typhon
def find(id: int) -> str?:
    if id == 1:
        return "Alice"
    return None

let raw: str? = find(2)
if raw is not None:
    print(raw)                # narrowed to str

guard r = find(2) else: return
print(r)                      # narrowed to str
```

```python
# Emitted Python
def find(id: int) -> str | None:
    if id == 1:
        return "Alice"
    return None

raw: str | None = find(2)
if raw is not None:
    print(raw)

if find(2) is None:
    return
r: str = find(2)              # roughly; the emitter binds via a temp
print(r)
```

Narrowing forms recognised by the checker: `is None`, `is not None`, `isinstance(x, T)`, `guard`, early-return `if x is None: return`.

---

## Classes

### Plain class → dataclass

```python
# Typhon
class User:
    id: int
    name: str = "anon"
    email: str?
```

```python
# Emitted Python
from dataclasses import dataclass

@dataclass(slots=True)
class User:
    id: int
    name: str = "anon"
    email: str | None = None
```

`slots=True` is the default. Instances do not carry a per-object `__dict__`; typos at attribute write sites raise `AttributeError`.

### Frozen class

```python
# Typhon
class Point frozen:
    x: float
    y: float
```

```python
# Emitted Python
@dataclass(slots=True, frozen=True)
class Point:
    x: float
    y: float
```

Note: `frozen=True` only blocks **field reassignment**. Nested mutable containers can still be mutated. Use `tuple` / `frozenset` for stronger guarantees.

### `model` → Pydantic

```python
# Typhon
model ApiUser:
    id: int
    email: str
    name: str = "anon"
```

```python
# Emitted Python
from pydantic import BaseModel, ConfigDict

class ApiUser(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: int
    email: str
    name: str = "anon"
```

`extra="forbid"` is currently always-on. `[emit] model-extra` is on the roadmap.

### Methods via `impl`

```python
# Typhon
class User:
    id: int
    name: str
    email: str?

impl User:
    def display() -> str:
        return f"{name} <{email}>" if email is not None else name

    def is_admin() -> bool:
        return id == 0
```

```python
# Emitted Python
@dataclass(slots=True)
class User:
    id: int
    name: str
    email: str | None = None

    def display(self) -> str:
        return f"{self.name} <{self.email}>" if self.email is not None else self.name

    def is_admin(self) -> bool:
        return self.id == 0
```

`self` is inserted at desugar; references to fields and other methods are rewritten through `self.`.

### Methods via `extend`

```python
# domain/user.ty
class User:
    id: int
    name: str

# analytics/user_metrics.ty
extend User:
    def tracking_id() -> str:
        return f"user-{id:08d}"
```

Emits identically to having both `impl User:` blocks in one file. The desugarer collects them across the project.

### Extending built-ins

```python
# Typhon
extend str:
    def slug() -> str:
        return strip().lower().replace(" ", "-")

let title: str = "Hello World"
print(title.slug())
print("untyped".slug())              # AttributeError at runtime — no static `str` annotation
```

```python
# Emitted Python (sketch)
def __typhon_ext_str__slug(self: str) -> str:
    return self.strip().lower().replace(" ", "-")

title: str = "Hello World"
print(__typhon_ext_str__slug(title))
print("untyped".slug())              # untouched; falls back to native attribute lookup
```

Only call sites whose receiver has a static `str` annotation get rewritten. No monkey-patching.

---

## Sealed unions and `match`

```python
# Typhon
type Shape = Circle | Rectangle | Triangle

class Circle:
    radius: float
class Rectangle:
    width: float
    height: float
class Triangle:
    base: float
    height: float

def area(s: Shape) -> float:
    match s:
        case Circle(radius):       return 3.14159 * radius * radius
        case Rectangle(w, h):      return w * h
        case Triangle(b, h):       return 0.5 * b * h
```

```python
# Emitted Python
from dataclasses import dataclass

@dataclass(slots=True)
class Circle:
    radius: float
@dataclass(slots=True)
class Rectangle:
    width: float
    height: float
@dataclass(slots=True)
class Triangle:
    base: float
    height: float

Shape = Circle | Rectangle | Triangle

def area(s: Shape) -> float:
    match s:
        case Circle(radius):       return 3.14159 * radius * radius
        case Rectangle(w, h):      return w * h
        case Triangle(b, h):       return 0.5 * b * h
```

Exhaustiveness is a compile-time check. `case _:` opts out for the rest of the union.

---

## `Result[T, E]`, `?`, `with`-chains

```python
# Typhon
def parse_port(raw: str) -> Result[int, str]:
    if not raw.isdigit():
        return Err(f"not a number: {raw}")
    let n: int = int(raw)
    if n < 1 or n > 65535:
        return Err(f"out of range: {n}")
    return Ok(n)

def parse_addr(host: str, raw: str) -> Result[tuple[str, int], str]:
    let port: int = parse_port(raw)?
    return Ok((host, port))
```

```python
# Emitted Python
from typhon_runtime import Ok, Err, Result

def parse_port(raw: str) -> Result[int, str]:
    if not raw.isdigit():
        return Err(f"not a number: {raw}")
    n: int = int(raw)
    if n < 1 or n > 65535:
        return Err(f"out of range: {n}")
    return Ok(n)

def parse_addr(host: str, raw: str) -> Result[tuple[str, int], str]:
    _tmp_0 = parse_port(raw)
    if isinstance(_tmp_0, Err):
        return _tmp_0
    port: int = _tmp_0.value
    return Ok((host, port))
```

`?` does **not** lower to `try/except`. The inline `isinstance(Err)` check is part of the design — stack traces stay clean.

`with`-chain:

```python
# Typhon
def make(uid: int) -> Result[Report, AppError]:
    with user   = db.find(uid)?,
         perms  = check(user)?,
         report = build(user, perms)?:
        return Ok(report)
    else err:
        log.warn(err)
        return Err(err)
```

```python
# Emitted Python (sketch)
def make(uid: int) -> Result[Report, AppError]:
    _r0 = db.find(uid)
    if isinstance(_r0, Err):
        err = _r0.error
        log.warn(err)
        return Err(err)
    user = _r0.value

    _r1 = check(user)
    if isinstance(_r1, Err):
        err = _r1.error
        log.warn(err)
        return Err(err)
    perms = _r1.value

    _r2 = build(user, perms)
    if isinstance(_r2, Err):
        err = _r2.error
        log.warn(err)
        return Err(err)
    report = _r2.value

    return Ok(report)
```

`Ok` and `Err` are emitted in `typhon_runtime.py`:

```python
@dataclass(slots=True, frozen=True)
class Ok(Generic[T]):
    value: T

@dataclass(slots=True, frozen=True)
class Err(Generic[E]):
    error: E
```

---

## Async and concurrency

### `gather:`

```python
# Typhon
async def load(uid: int) -> Dashboard:
    gather:
        user   = fetch_user(uid)
        posts  = fetch_posts(uid)
        notifs = fetch_notifs(uid)
    return Dashboard(user=user, posts=posts, notifs=notifs)
```

```python
# Emitted Python
import asyncio

async def load(uid: int) -> Dashboard:
    async with asyncio.TaskGroup() as _tg:
        _t_user   = _tg.create_task(fetch_user(uid))
        _t_posts  = _tg.create_task(fetch_posts(uid))
        _t_notifs = _tg.create_task(fetch_notifs(uid))
    user   = _t_user.result()
    posts  = _t_posts.result()
    notifs = _t_notifs.result()
    return Dashboard(user=user, posts=posts, notifs=notifs)
```

Best-effort variant:

```python
# Typhon
gather(strategy="best-effort"):
    user = fetch_user(uid)
    posts = fetch_posts(uid)
```

```python
# Emitted Python
user, posts = await asyncio.gather(
    fetch_user(uid),
    fetch_posts(uid),
    return_exceptions=True,
)
# user / posts now have type `User | BaseException` etc.
```

### `go`

```python
# Typhon
async def signup(email: str) -> User:
    let user: User = await create(email)
    go send_welcome(user)
    return user

async def signup_with_handle(email: str) -> User:
    let user: User = await create(email)
    go send_welcome(user) -> task
    await task
    return user
```

```python
# Emitted Python (sketch)
from typhon_runtime.tasks import spawn

async def signup(email: str) -> User:
    user: User = await create(email)
    spawn(send_welcome(user))
    return user

async def signup_with_handle(email: str) -> User:
    user: User = await create(email)
    task = spawn(send_welcome(user))
    await task
    return user
```

`spawn` registers the task in a strong-ref registry and clears the entry from a done-callback. Never use `asyncio.create_task` directly — weak refs let fire-and-forget tasks be GC'd mid-flight.

---

## Lazy

```python
# Typhon
lazy import np = numpy

def main() -> None:
    if len(sys.argv) > 1:
        let arr: np.ndarray = np.array([1, 2, 3])
```

```python
# Emitted Python (sketch)
import importlib.util
import sys as _sys

_spec = importlib.util.find_spec("numpy")
_loader = importlib.util.LazyLoader(_spec.loader)
_spec.loader = _loader
np = importlib.util.module_from_spec(_spec)
_sys.modules["numpy"] = np
_loader.exec_module(np)         # deferred — only runs on first attribute access via the proxy
```

The exact emission uses a small thread-safe proxy class so concurrent first accesses serialise around the underlying load.

`lazy from numpy import array` is **rejected at parse time** (`tyc::lazy_from`). Use `lazy import numpy` and dotted access.

```python
# Module-level lazy let
lazy let CONFIG: Config = load_config_from_disk()
```

```python
# Emitted Python (sketch)
from typhon_runtime.lazy import lazy_let as __typhon_lazy_let

CONFIG: Config = __typhon_lazy_let(lambda: load_config_from_disk())
```

Inside a class body, `lazy let x: T = expr` lowers to `@cached_property`.

`lazy[list[T]]` return type:

```python
# Typhon
def primes(n: int) -> lazy[list[int]]:
    ...
    return [i for i in range(2, n + 1) if sieve[i]]
```

```python
# Emitted Python
def primes(n: int) -> Iterator[int]:
    ...
    yield from (i for i in range(2, n + 1) if sieve[i])
```

The emitter rewrites the function body as a generator so the caller can consume a prefix without materialising the whole list.

---

## `comptime`

```python
# Typhon
comptime let PORT: int = int(env("PORT", "8080"))
comptime let DB_URL: str = env("DATABASE_URL")

comptime def feature(name: str) -> bool:
    return env(f"FEATURE_{name.upper()}", "0") == "1"

comptime let DARK_MODE: bool = feature("dark_mode")
```

```python
# Emitted Python (literals inlined at build time)
PORT: int = 8080
DB_URL: str = "postgresql://..."
DARK_MODE: bool = True
```

The sandboxed interpreter allows pure arithmetic, string ops, container construction, `env(name, default?)`, and calls to other `comptime` functions. It forbids I/O, subprocess, network, random/time, arbitrary imports.

Required env vars are declared in `typhon.toml`:

```toml
[env]
required = ["DATABASE_URL"]
```

Missing required env → build fails with `tyc::comptime_env_missing`.

---

## Generics — PEP 695

```python
# Typhon
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]

class Box[T]:
    value: T

impl[T] Box[T]:
    def get() -> T:
        return value

    def map[U](f: Callable[[T], U]) -> Box[U]:
        return Box(value=f(value))

type Vec[T] = list[T]
type Pair[A, B] = tuple[A, B]
```

```python
# Emitted Python (PEP 695 preserved on 3.12+ targets)
def first[T](xs: list[T]) -> T | None:
    if len(xs) == 0:
        return None
    return xs[0]

@dataclass(slots=True)
class Box[T]:
    value: T

    def get(self) -> T:
        return self.value

    def map[U](self, f: Callable[[T], U]) -> "Box[U]":
        return Box(value=f(self.value))

type Vec[T] = list[T]
type Pair[A, B] = tuple[A, B]
```

Bound: `def smallest[T: Ordered](xs: list[T]) -> T?:` — bounded type-vars are listed as partial in the current implementation. Single-argument bounds work; multi-argument constraint solving is still landing.

**Never** import `TypeVar` from `typing`. The PEP 695 path is the only one.

---

## Interfaces

```python
# Typhon
interface Drawable:
    def draw() -> None
    def width() -> float
    def height() -> float

class Button:
    label: str

impl Button:
    def draw() -> None: print(label)
    def width() -> float: return 10.0
    def height() -> float: return 1.0

def render(d: Drawable) -> None:
    d.draw()
```

```python
# Emitted Python
from typing import Protocol

class Drawable(Protocol):
    def draw(self) -> None: ...
    def width(self) -> float: ...
    def height(self) -> float: ...

@dataclass(slots=True)
class Button:
    label: str

    def draw(self) -> None:
        print(self.label)
    def width(self) -> float:
        return 10.0
    def height(self) -> float:
        return 1.0

def render(d: Drawable) -> None:
    d.draw()
```

Conformance is verified structurally at the call site. `isinstance(x, Drawable)` is rejected by default (`tyc::interface_isinstance`).

---

## Pipes and guards

```python
# Typhon
let cleaned: str = raw |> str.strip() |> str.lower() |> str.replace(",", "")
let result = x |> normalise() |> scale(2.0) |> clamp(0.0, 1.0)
```

```python
# Emitted Python
cleaned: str = str.replace(str.lower(str.strip(raw)), ",", "")
result = clamp(scale(normalise(x), 2.0), 0.0, 1.0)
```

Left-associative; `a |> f(arg)` is exactly `f(a, arg)`. The piped value fills the *first* positional slot.

```python
# Typhon
def shipping(weight: float?) -> float:
    guard w = weight else:
        return 0.0
    return w * 1.25
```

```python
# Emitted Python
def shipping(weight: float | None) -> float:
    if weight is None:
        return 0.0
    w: float = weight
    return w * 1.25
```

The `guard ... else:` block must return / raise / otherwise leave the enclosing function.

---

## Purity and memo

```python
# Typhon
@pure
def normalise(s: str) -> str:
    return s.strip().lower()

@memo
def fib(n: int) -> int:
    if n < 2: return n
    return fib(n - 1) + fib(n - 2)

@memo(max=128)
def expensive(k: str) -> int: ...
```

```python
# Emitted Python
import functools

def normalise(s: str) -> str:
    return s.strip().lower()

@functools.cache
def fib(n: int) -> int:
    if n < 2: return n
    return fib(n - 1) + fib(n - 2)

@functools.lru_cache(maxsize=128)
def expensive(k: str) -> int: ...
```

`@pure` alone emits nothing — it's a static assertion. `@memo` (or `@pure(memo=True)`) inserts `functools.cache` / `lru_cache`. Manual `@pure` on a function failing any of the six purity conditions is `tyc::impure_pure_fn`.

---

## `unsafe:` boundary

```python
# Typhon
def parse() -> int:
    unsafe:
        let v = mystery_lib.get_int()
        let checked: int = int(v)
    return checked
```

```python
# Emitted Python
def parse() -> int:
    if True:                       # scope-preserving lowering
        v = mystery_lib.get_int()
        checked: int = int(v)
    return checked
```

The checker tracks an `unsafe_depth` counter, suppresses `tyc::implicit_any` inside the block, and marks every binding as `Unsafe[T]`. An `Unsafe[T]` cannot cross out of the block into a non-`unsafe` context expecting a concrete `T` — re-assert with an annotated `let`/`mut`, narrow, or cast.

---

## Source maps

`tyc build` emits `build/*.py` and `build/*.py.map`. The map is **v2**: a per-statement `(out_line → ty_line)` table that `tyc trace` consumes to rewrite Python tracebacks back to `.ty` source. Pair `tyc debug` with `tyc trace` — frames in the debugger surface as `build/*.py` paths, and `tyc trace` remaps them.

`.py.map` is also consumed by the LSP for go-to-definition that crosses the `.ty` ↔ `.py` boundary.
