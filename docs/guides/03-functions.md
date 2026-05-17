# 3. Functions

Functions in Typhon look like typed Python functions, with two extra rules: every parameter and return type must be annotated, and the checker enforces those annotations everywhere the function is called.

## The shape of a function

```python
def add(a: int, b: int) -> int:
    return a + b
```

Three required pieces:

- A name (`add`).
- Annotated parameters (`a: int, b: int`).
- A return type (`-> int`).

Omit any of them and `tyc check` complains.

## Default arguments

Defaults work the way they do in Python, but the default value must match the annotation:

```python
def greet(name: str = "world", punctuation: str = "!") -> str:
    return f"Hello, {name}{punctuation}"

greet()                  # "Hello, world!"
greet("Alice")           # "Hello, Alice!"
greet(punctuation=".")   # "Hello, world."
```

A mismatched default is rejected at the definition site:

```python
def bad(n: int = "zero") -> int: ...    # ❌ type mismatch
```

## Returning nothing

`-> None` is the explicit form. Don't omit the annotation hoping it'll be inferred — that's a hard error.

```python
def log(msg: str) -> None:
    print(f"[log] {msg}")
```

A function declared `-> None` may use `return` with no value, or run off the end. It may not return any other value.

## Returning multiple values

Use a tuple; the call site destructures with normal Python syntax:

```python
def divmod_pair(a: int, b: int) -> tuple[int, int]:
    return a // b, a % b

let q, r = divmod_pair(17, 5)    # q = 3, r = 2
```

If two of those values mean different things, prefer a small `class` or `model` (guide 5) — readers shouldn't have to remember which slot is which.

## Optional parameters via `T?`

A nullable parameter type lets the caller pass `None`. It does **not** auto-default to `None`:

```python
def find(name: str?) -> int?:
    if name is None:
        return None
    return len(name)

find(None)       # ✅
find("alice")    # ✅
find()           # ❌ missing positional argument
```

If you want both — nullable *and* defaulted — be explicit:

```python
def find(name: str? = None) -> int?:
    ...
```

## Keyword-only and positional-only parameters

Python's `*` and `/` separators work unchanged:

```python
def connect(host: str, /, *, port: int = 5432, ssl: bool = True) -> None:
    ...

connect("localhost", port=5433)        # ✅
connect(host="localhost")              # ❌ host is positional-only
connect("localhost", 5433)             # ❌ port is keyword-only
```

## `*args` and `**kwargs`

Allowed, but each must be annotated:

```python
def log_all(*messages: str, **tags: str) -> None:
    let joined: str = ", ".join(messages)
    let tag_pairs: str = " ".join(f"{k}={v}" for k, v in tags.items())
    print(f"[log] {joined} ({tag_pairs})")

log_all("start", "ready", env="prod", region="eu")
```

Inside the body, `messages` has type `tuple[str, ...]` and `tags` has type `dict[str, str]`.

## First-class functions

Functions are values. Annotate them with `Callable`:

```python
from collections.abc import Callable

def apply(f: Callable[[int], int], x: int) -> int:
    return f(x)

def double(n: int) -> int:
    return n * 2

apply(double, 7)    # 14
```

`Callable[[A, B], R]` is "takes `A` and `B`, returns `R`". For a no-arg callable, use `Callable[[], R]`.

## Lambdas

Single-expression anonymous functions; the parameter and return types are inferred from context:

```python
let nums: list[int] = [1, 2, 3, 4]
let squares: list[int] = [x * x for x in nums]

# Or with map:
let doubled: list[int] = list(map(lambda n: n * 2, nums))
```

If the context can't pin down the parameter type, you'll get an "implicit Any" error. Promote to a named function when that happens — lambdas are best kept short.

## Generic functions

Typhon uses **PEP 695 syntax** for generics (`def f[T](...)`). Call-site inference fills in `T`:

```python
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]

let n: int? = first([1, 2, 3])       # T inferred as int
let s: str? = first(["a", "b"])      # T inferred as str
```

Generics are covered in depth in [guide 8](08-generics-and-interfaces.md).

## Common mistakes

**Missing return annotation:**

```python
def add(a: int, b: int):
    return a + b
```

```
error[tyc::missing_return_type]: function `add` is missing a return type
```

Fix: `def add(a: int, b: int) -> int:`.

**Inconsistent return types:**

```python
def lookup(k: str) -> int:
    if k == "":
        return None       # ❌ None is not assignable to int
    return 42
```

Fix the annotation (`-> int?`) or the return value.

**Calling a function with the wrong arity:**

```python
def add(a: int, b: int) -> int: ...

add(1)          # ❌ missing argument `b`
add(1, 2, 3)    # ❌ too many arguments
```

The diagnostic points at the call site, not the definition.

**Forgetting to `await` an async function:**

```python
async def fetch() -> str: ...

def main() -> None:
    let s: str = fetch()    # ❌ coroutine, not str
```

This is a hard error in Typhon — not a runtime warning. (Async details in [guide 9](09-async-and-concurrency.md).)

## Putting it together

A small CLI-style example:

```python
import sys

def parse_args(argv: list[str]) -> tuple[str, int]:
    let name: str = argv[1] if len(argv) > 1 else "world"
    let times: int = int(argv[2]) if len(argv) > 2 else 1
    return name, times

def greet(name: str, times: int = 1) -> None:
    for _ in range(times):
        print(f"Hello, {name}")

def main() -> None:
    let name, times = parse_args(sys.argv)
    greet(name, times)

if __name__ == "__main__":
    main()
```

Run it:

```bash
tyc build
python build/main.py Alice 3
# Hello, Alice
# Hello, Alice
# Hello, Alice
```

## What you've learned

- Every parameter and return type is annotated; the checker enforces both.
- Defaults, `*args`, `**kwargs`, and keyword-only parameters work as in Python.
- `Callable[[...], R]` types first-class functions.
- Generic functions use `def f[T](...)` (PEP 695 syntax).
- Mismatched arities, returns, and missing awaits are all compile errors.

Next: [Control flow and collections](04-control-flow-and-collections.md) — `if`/`while`/`for`, lists, dicts, sets, tuples, comprehensions, and the `guard` statement.
