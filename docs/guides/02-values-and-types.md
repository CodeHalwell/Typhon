# 2. Values and types

Two ideas do most of the safety work in Typhon: **bindings are immutable by default**, and **types cannot hold `None` unless you say so**. Everything in this guide flows from those two rules.

## `let` and `mut`

A local binding picks one of two keywords:

```python
def demo() -> None:
    let pi: float = 3.14159
    mut counter: int = 0

    counter = counter + 1    # ✅ mut is reassignable
    # pi = 3.14              # ❌ compile error: `pi` is a `let`
```

- **`let`** — immutable binding. Reassignment is a compile error.
- **`mut`** — mutable binding. Required for any name you intend to rebind.

This is *binding* immutability, like Rust's `let` vs `let mut` or TypeScript's `const` vs `let`. A `let` cannot point at a new object, but the object it points at can still have mutable fields. For deep immutability, freeze the underlying dataclass (see [guide 5](05-classes-and-models.md)).

### Why both? Why default to `let`?

Mutability is a hazard you want to opt into, not out of. `let` is the cheaper-to-read choice for the reader: they don't have to scan the rest of the function looking for reassignments. Reach for `mut` only when you actually need it (loop counters, accumulators, builders).

### Module-level bindings default to `let`

```python
PI: float = 3.14159           # implicitly `let`
mut feature_flag: bool = False  # explicitly mutable
```

Inside a function, the kind is always explicit. At module top level, it's `let` unless declared otherwise.

## Primitives

The familiar Python primitives, with stricter rules:

| Type | Example | Notes |
|------|---------|-------|
| `int` | `42` | Arbitrary precision, like Python |
| `float` | `3.14` | 64-bit IEEE 754 |
| `bool` | `True` / `False` | Not an `int` for type-checking purposes |
| `str` | `"hello"` | UTF-8, identical to Python |
| `bytes` | `b"hi"` | Immutable byte sequences |
| `None` | `None` | Inhabitant of the unit type; only valid where `T?` allows it |

```python
def types() -> None:
    let n: int = 10
    let ratio: float = n / 3       # int → float, allowed
    let msg: str = f"n={n}"
    let flag: bool = n > 0
```

### `int` and `float` are distinct

```python
let x: int = 3.14    # ❌ type mismatch: expected int, found float
let y: float = 3     # ✅ int is assignable to float (widening)
```

Floats do not silently truncate to ints. Use `int(x)` or `round(x)` explicitly.

## Non-nullable by default

Plain `T` cannot hold `None`. Optional values use `T?`, which is sugar for `T | None`:

```python
def find_user(id: int) -> str?:
    if id == 1:
        return "Alice"
    return None

def greet(name: str) -> None:
    print(f"Hello, {name}")

def main() -> None:
    let found: str? = find_user(42)
    # greet(found)              # ❌ `str?` is not assignable to `str`
```

The compiler tells you exactly why:

```
error[tyc::nullable_use]: cannot pass `str | None` where `str` is required
 ┌─ src/main.ty:9:11
 │
9 │     greet(found)
 │           ^^^^^ check this is not `None` before passing it
```

### Flow narrowing

Once you check, the type narrows. Inside the `if` branch, `found` is `str`, not `str?`:

```python
def main() -> None:
    let found: str? = find_user(42)
    if found is not None:
        greet(found)             # ✅ narrowed to str
    else:
        greet("stranger")
```

`is None`, `is not None`, and `isinstance(x, T)` all narrow. So does early-return:

```python
def main() -> None:
    let found: str? = find_user(42)
    if found is None:
        return
    greet(found)                 # ✅ everything after the guard sees `str`
```

### `guard` for early-return narrowing

A common pattern — bail out on `None`, then use the value — has dedicated sugar:

```python
def handle(id: int) -> None:
    guard name = find_user(id) else:
        print("not found")
        return
    greet(name)                  # `name` narrowed to `str`
```

`guard` is covered in more depth in [guide 4](04-control-flow-and-collections.md).

## How `T?` emits

Typhon stores nullability internally as `Nullable[T]`, but emits the standard Python form:

```python
# Typhon
def find_user(id: int) -> str?: ...

# Emitted Python
def find_user(id: int) -> str | None: ...
```

Existing Python tooling (mypy, pyright, IDEs) sees `str | None` and handles it exactly as you'd expect.

## Implicit `Any` — the convention

Typhon's long-term intent is to refuse implicit `Any` outside an `unsafe:` region — stricter than TypeScript's `noImplicitAny`. Today the type system *allows* `Any` to flow freely (it's the top type), so an unconstrained import binds silently:

```python
import some_untyped_lib

def main() -> None:
    let data = some_untyped_lib.fetch()    # binds to Any silently
```

The recommended convention is one of the two patterns below — write them yourself; reviewers will start expecting them, and a future release will enforce them:

```python
# Option A: assert the type with an annotation
let data: dict[str, int] = some_untyped_lib.fetch()

# Option B: wrap in `unsafe` (acknowledges the dynamic boundary)
unsafe:
    let data = some_untyped_lib.fetch()
```

`unsafe` is the right tool when you genuinely don't know the type — e.g. exploring a new dependency. For production code, an annotation or a `.dty` stub (guide 10) is cleaner.

## Putting it together

A tiny example that uses everything in this guide:

```python
def parse_port(raw: str?) -> int? :
    guard raw = raw else:
        return None
    if raw.isdigit():
        return int(raw)
    return None

def main() -> None:
    import os
    let port: int? = parse_port(os.environ.get("PORT"))
    mut host: str = "localhost"

    if port is None:
        print(f"using default port on {host}")
    else:
        print(f"binding {host}:{port}")
```

Walk through what's happening:

1. `parse_port` takes a `str?` and returns an `int?`. Both can be `None`.
2. `guard raw = raw` shadows `raw` with the narrowed `str` inside the function body.
3. `os.environ.get(...)` returns `str | None` — Python sees this and the types line up.
4. The `if port is None` check narrows `port` to `int` in the `else` branch, so the f-string is safe.
5. `host` is `mut` because... well, in this example it isn't reassigned, so we could have used `let`. The compiler won't complain about over-mutability, but readers will.

## What you've learned

- **`let`** is for bindings you don't intend to rebind; **`mut`** opts into mutability.
- **`T`** never holds `None`; **`T?`** does, and the checker tracks it.
- **Flow narrowing** lets you use `T?` values safely once you've checked them — `if`, `is None`, `isinstance`, `guard`.
- **No implicit `Any`** — either annotate or wrap in `unsafe`.

Next: [Functions](03-functions.md) — signatures, parameters, return types, and the rules around them.
