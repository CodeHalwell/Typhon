# Typhon — Extended Pitfalls Catalogue

The errors and surprises that bite people who try to write Typhon as if it were Python. Ranked roughly by frequency, with the diagnostic you'll see and the fix.

For each entry: **trigger → diagnostic → fix**.

---

## 1. Forgetting the return type

```python
def main():
    print("hi")
```

```
error[tyc::missing_return_type]: function `main` is missing a return type
 ┌─ src/main.ty:1:5
 │
1 │ def main():
 │     ^^^^ add `-> None` (or another type) here
```

**Fix:** `def main() -> None:`.

`[strictness] no-implicit-any = true` is the default. Even functions that return nothing need `-> None` — there is no inference fallback.

---

## 2. Function-local `=` without `let`/`mut`

```python
def main() -> None:
    name = "Alice"
    print(name)
```

```
error[tyc::missing_binding_kind]: local bindings must be declared with `let` or `mut`
 ┌─ src/main.ty:2:5
 │
2 │     name = "Alice"
 │     ^^^^ add `let` (immutable) or `mut` (mutable) here
```

**Fix:** `let name: str = "Alice"` or `mut name: str = "Alice"`.

Module-level annotated assignments are fine without the keyword (they default to `let`). The rule is for *function-local* bindings only.

---

## 3. Reassigning a `let`

```python
def demo() -> None:
    let count: int = 0
    count = count + 1
```

```
error[tyc::let_reassign]: cannot reassign `let` binding `count`
 ┌─ src/main.ty:3:5
 │
3 │     count = count + 1
 │     ^^^^^ declare with `mut` to allow reassignment
```

**Fix:** `mut count: int = 0`.

---

## 4. Passing `T?` where `T` is required

```python
def greet(name: str) -> None: ...
def find(id: int) -> str?: ...

let raw: str? = find(1)
greet(raw)
```

```
error[tyc::nullable_use]: cannot pass `str | None` where `str` is required
 ┌─ src/main.ty:5:7
 │
5 │ greet(raw)
 │       ^^^ check this is not `None` before passing it
```

**Fix:** narrow first.

```python
if raw is not None:
    greet(raw)

# or
guard r = raw else: return
greet(r)
```

---

## 5. Putting methods inside `class`

```python
class User:
    id: int

    def display(self) -> str:
        return f"user {self.id}"
```

`class` declarations are minimalist: no `__init__`, no methods. Methods live in `impl`:

```python
class User:
    id: int

impl User:
    def display() -> str:
        return f"user {id}"
```

The desugarer merges the `impl` block in and inserts `self`. Forgetting `self` is impossible because there *is* no `self` to forget.

---

## 6. Writing `__init__`

```python
class User:
    id: int

    def __init__(self, id: int) -> None:
        self.id = id
```

```
error[tyc::manual_init]: classes do not declare `__init__`; the constructor is generated
```

**Fix:** drop it. Use field defaults for "convenience constructors", or write a free function (`def make_user(...) -> User: ...`).

---

## 7. Using `TypeVar` instead of PEP 695

```python
from typing import TypeVar
T = TypeVar("T")
def first(xs: list[T]) -> T | None: ...
```

Use PEP 695 syntax:

```python
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]
```

Typhon does not accept the `TypeVar` import path.

---

## 8. `isinstance` against an interface

```python
interface Drawable:
    def draw() -> None

def render_if_able(x: object) -> None:
    if isinstance(x, Drawable):
        x.draw()
```

```
error[tyc::interface_isinstance]: runtime `isinstance` against an interface is unsafe
                                  by default; use static narrowing or opt into
                                  `@runtime_checkable` explicitly
```

Python's `@runtime_checkable` only checks attribute *presence*, not signatures — so an object with a `draw` field that isn't callable would pass. Typhon refuses.

**Fix options:**

- Refactor to a sealed union if the variants are closed.
- Write an explicit predicate function with hasattr+callable checks.
- Static narrowing via a parameter typed as the interface.

---

## 9. Using `asyncio.create_task` directly

```python
asyncio.create_task(send_welcome(user))
```

Python's event loop holds **weak** refs to tasks. A fire-and-forget task whose handle is dropped can be GC'd mid-flight.

**Fix:** `go send_welcome(user)`. Lowers through `typhon_runtime.tasks.spawn`, which keeps a strong-ref registry.

---

## 10. `lazy from foo import bar`

```python
lazy from numpy import array
```

```
error[tyc::lazy_from]: `lazy from ... import ...` is rejected — `from` imports eagerly
                       touch attributes on the source module and defeat deferral
```

**Fix:** `lazy import numpy`, then `numpy.array(...)`.

---

## 11. `comptime` reading runtime-only values

```python
comptime let NOW: float = time.time()
```

```
error[tyc::comptime_violation]: `comptime` sandbox forbids `time.time`
                                (non-deterministic clock)
```

The comptime sandbox allows: pure arithmetic, string ops, `env(name, default?)`, simple containers, calls to other comptime functions. It forbids: I/O, subprocess, network, random, time, arbitrary imports.

**Fix:** compute at runtime, cache with `lazy let`:

```python
lazy let STARTUP_TIME: float = time.time()
```

---

## 12. Missing variants in a sealed-union `match`

```python
type Shape = Circle | Rectangle | Triangle

def area(s: Shape) -> float:
    match s:
        case Circle(r):       return 3.14159 * r * r
        case Rectangle(w, h): return w * h
        # forgot Triangle
```

```
error[tyc::non_exhaustive_match]: match does not cover variant `Triangle`
```

**Fix:** add the missing case, or use `case _:` if a deliberate catch-all is the intent. Using `case _:` opts out of exhaustiveness for the rest of the union — use sparingly.

---

## 13. `?` outside a `Result`-returning function

```python
def main() -> None:
    let port: int = parse_port("8080")?
```

```
error[tyc::result_propagate_outside_result]: `?` can only short-circuit inside a function
                                              returning a compatible `Result`
```

**Fix:** change the signature (`-> Result[int, str]`) or `match` explicitly.

---

## 14. Mismatched error types across `?`

```python
def parse_port(raw: str) -> Result[int, str]: ...

def boot(raw: str) -> Result[int, ConfigError]:
    let port: int = parse_port(raw)?
```

```
error[tyc::result_error_mismatch]: cannot propagate `Err[str]` into a function
                                   returning `Result[int, ConfigError]`
```

**Fix:** convert at the boundary:

```python
match parse_port(raw):
    case Ok(port):
        return Ok(port)
    case Err(msg):
        return Err(BadPort(raw=raw))
```

---

## 15. `@pure` on an impure function

```python
@pure
def fetch(url: str) -> str:
    import urllib.request
    return urllib.request.urlopen(url).read().decode()
```

```
error[tyc::pure_violation]: `fetch` is annotated `@pure` but performs I/O
                            (urllib.request.urlopen)
```

The six purity conditions: synchronous, hashable params, no I/O, no non-determinism, no mutable module state, no exceptions raised. Failing *any* condition is a hard error.

**Fix:** drop `@pure` (and lose memo eligibility), or refactor the I/O out.

---

## 16. Bare `list` / `dict` annotation

```python
let xs: list = [1, 2, 3]
```

```
error[tyc::implicit_any]: list element type is `Any`; provide a parameter
```

**Fix:** `let xs: list[int] = [1, 2, 3]`.

Same for `dict`, `set`, `tuple`. Element / value types are mandatory under `[strictness] no-implicit-any = true` (the default).

---

## 17. `dict.get(k)` as if it returned `V`

```python
let counts: dict[str, int] = {"a": 1}
let n: int = counts.get("missing")
```

`get(...)` returns `V?`. Either narrow:

```python
let n: int? = counts.get("missing")
if n is None:
    return
print(n)
```

…or use `d[k]` (typed `V`, may raise `KeyError`).

---

## 18. `bool` treated as `int`

```python
def takes_int(n: int) -> None: ...
let flag: bool = True
takes_int(flag)
```

```
error[tyc::type_mismatch]: expected `int`, found `bool`
```

Cast explicitly: `takes_int(int(flag))`. Typhon does not subtype `bool <: int`.

---

## 19. Mismatched default arg

```python
def bad(n: int = "zero") -> int: ...
```

```
error[tyc::default_mismatch]: default value type `str` does not match annotation `int`
```

**Fix:** match the type, or change the annotation.

---

## 20. `Unsafe[T]` leaking out of `unsafe:`

```python
def parse() -> int:
    unsafe:
        let v = mystery_lib.get_int()
    return v
```

```
error[tyc::unsafe_leak]: cannot return `Unsafe[Any]` from a function declared `-> int`
```

`Unsafe[T]` is a hidden marker — visible in diagnostics, not in source. Re-assert at the boundary:

```python
def parse() -> int:
    unsafe:
        let v = mystery_lib.get_int()
        let checked: int = int(v)
    return checked
```

---

## 21. Empty list with unconstrained generic

```python
let empty = first([])
```

```
error[tyc::implicit_any]: cannot infer `T` for `first` — argument has no element type
```

**Fix:** annotate at the call: `let empty: int? = first[int]([])`, or `let empty: int? = first([] : list[int])`.

---

## 22. `async def` with no `await`

```python
async def helper() -> int:
    return 42
```

```
warning[tyc::async_without_await]: `helper` is `async` but never awaits;
                                   make it sync, or `await` something inside it
```

Almost always a mistake. Drop the `async` or `await` something. Warning-level by default.

---

## 23. Forgetting `await`

```python
async def fetch(url: str) -> str: ...

def main() -> None:
    let body: str = fetch("...")
```

```
error[tyc::missing_await]: cannot use a coroutine where `str` is required
```

**Fix:** `let body: str = await fetch("...")` and make `main` `async`.

---

## 24. Blocking I/O inside `async def`

```python
async def load() -> bytes:
    with open("data.bin", "rb") as f:
        return f.read()
```

Typhon does **not** catch this (it's a Python-wide hazard). Use `aiofiles` or wrap with `asyncio.to_thread(...)`.

This is in the catalogue so you remember to check it during code review.

---

## 25. Mistaking dataclass `frozen=True` for deep immutability

```python
class Box frozen:
    items: list[int]

let b: Box = Box(items=[1, 2, 3])
b.items.append(4)        # ✅ allowed — nested list is mutable
b.items = []             # ❌ tyc::frozen_assign — field reassignment blocked
```

Dataclass `frozen=True` blocks **field reassignment**, not nested mutation. For deep immutability, use immutable containers inside (`tuple`, `frozenset`).

---

## 26. Stub drift between `.dty` and runtime

```
error[tyc::stub_mismatch]: stub declares `Redis.set(key, value) -> bool` but runtime
                          signature is `set(name, value, ex=None) -> bool`
```

Raised by `tyc check --stubs`. For in-tree stubs (you wrote both), it's a code-review fail. For third-party stubs, it usually means the library was upgraded — update the stub and re-check.

---

## 27. `with`-chain in a non-`Result` function

```python
def main() -> None:
    with user = db.find(1)?, perms = check(user)?:
        return Ok(...)
    else err:
        return Err(err)
```

```
error[tyc::result_propagate_outside_result]: `?` inside a `with`-chain requires
                                              an enclosing `Result`-returning function
```

**Fix:** lift to a helper that returns `Result`, or write the chain without `?` and `match` each step.

---

## 28. `gather:` with dependent values

```python
gather:
    user = fetch_user(uid)
    posts = fetch_posts(user.id)   # depends on `user`
```

```
error[tyc::gather_dependency]: `fetch_posts` references `user`, but bindings
                               inside `gather:` are awaited in parallel
```

`gather:` requires each binding to be **independent**. Move dependent calls outside:

```python
let user: User = await fetch_user(uid)
gather:
    posts = fetch_posts(user.id)
    notifs = fetch_notifs(user.id)
```

---

## 29. Module-level `lazy let` consumed inside a hot loop

```python
lazy let CONFIG: Config = load_config_from_disk()

def serve(n: int) -> None:
    for _ in range(n):
        if CONFIG.feature_x:
            ...
```

The first access pays the load cost; subsequent accesses are memory reads — but the runtime helper still goes through a `__getattr__` shim. For very hot inner loops, hoist:

```python
def serve(n: int) -> None:
    let feature_x: bool = CONFIG.feature_x
    for _ in range(n):
        if feature_x:
            ...
```

Not a correctness issue — a perf hint.

---

## 30. Missing required env var at build time

```toml
[env]
required = ["DATABASE_URL"]
```

```python
comptime let DB_URL: str = env("DATABASE_URL")
```

If `DATABASE_URL` is unset:

```
error[tyc::comptime_env_missing]: required env var `DATABASE_URL` is unset
                                   (declared in [env] required)
```

`tyc build` fails at compile time, not at runtime. This is feature, not bug — set the env var or remove it from `[env] required`.

---

## Quick "which guide answers this" map

| If you're stuck on… | Read |
|---|---|
| `let`/`mut`, `T?`, narrowing | `docs/guides/02-values-and-types.md` |
| Function rules, `Callable`, generics syntax | `docs/guides/03-functions.md` |
| `guard`, comprehensions, collections | `docs/guides/04-control-flow-and-collections.md` |
| `class` / `model` / `impl` / `extend` / `frozen` | `docs/guides/05-classes-and-models.md` |
| `Result`, `?`, `with`-chains | `docs/guides/06-error-handling.md` |
| Sealed unions, exhaustive match | `docs/guides/07-sealed-unions-and-match.md` |
| Generics, interfaces, structural typing | `docs/guides/08-generics-and-interfaces.md` |
| `async`, `gather:`, `go`, free-threaded | `docs/guides/09-async-and-concurrency.md` |
| Pipes, `comptime`, `lazy`, `@pure`/`@memo`, `unsafe`, `.dty` | `docs/guides/10-advanced-features.md` |
| The "why" of any design call | `docs/long-term-plan.md` |
| Compiler internals / crate layout | `docs/architecture.md` |
| `typhon.toml` keys | `docs/configuration.md` |
| `tyc` flags | `docs/cli.md` |
