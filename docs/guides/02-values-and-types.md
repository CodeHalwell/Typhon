# 2. Values and types

Two ideas do most of the safety work in Typhon: **bindings are immutable by default**, and **types cannot hold `None` unless you say so**. Everything in this guide flows from those two rules.

## `val` and `var`

A local binding picks one of two keywords:

```python
def demo() -> None:
    val pi: float = 3.14159
    var counter: int = 0

    counter = counter + 1    # ✅ var is reassignable
    # pi = 3.14              # ❌ compile error: `pi` is a `val`
```

- **`val`** — immutable binding. Reassignment is a compile error.
- **`var`** — mutable binding. Required for any name you intend to rebind.

This is *binding* immutability, like Rust's `let` vs `let mut` or TypeScript's `const` vs `let`. A `val` cannot point at a new object, but the object it points at can still have mutable fields. For deep immutability, freeze the underlying dataclass (see [guide 5](05-classes-and-models.md)).

### Why both? Why default to `val`?

Mutability is a hazard you want to opt into, not out of. `val` is the cheaper-to-read choice for the reader: they don't have to scan the rest of the function looking for reassignments. Reach for `var` only when you actually need it (loop counters, accumulators, builders).

### Module-level bindings default to `val`

```python
PI: float = 3.14159           # implicitly `val`
var feature_flag: bool = False  # explicitly mutable
```

Inside a function, the kind is always explicit. At module top level, it's `val` unless declared otherwise.

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
    val n: int = 10
    val ratio: float = n / 3       # int → float, allowed
    val msg: str = f"n={n}"
    val flag: bool = n > 0
```

### `int` and `float` are distinct

```python
val x: int = 3.14    # ❌ type mismatch: expected int, found float
val y: float = 3     # ✅ int is assignable to float (widening)
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
    val found: str? = find_user(42)
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
    val found: str? = find_user(42)
    if found is not None:
        greet(found)             # ✅ narrowed to str
    else:
        greet("stranger")
```

`is None`, `is not None`, and `isinstance(x, T)` all narrow. So does early-return:

```python
def main() -> None:
    val found: str? = find_user(42)
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

## No implicit `Any`

Typhon refuses to infer `Any` outside an `unsafe` block. This is stricter than TypeScript's `noImplicitAny` — there's no per-value escape hatch, only the lexical region.

```python
import some_untyped_lib

def main() -> None:
    val data = some_untyped_lib.fetch()    # ❌ infers Any
```

```
error[tyc::implicit_any]: cannot infer a type for `data`
 ┌─ src/main.ty:4:9
 │
4 │     val data = some_untyped_lib.fetch()
 │         ^^^^ the right-hand side has type `Any`; annotate or wrap in `unsafe`
```

Two fixes — pick the one that fits:

```python
# Option A: assert the type with an annotation
val data: dict[str, int] = some_untyped_lib.fetch()

# Option B: wrap in `unsafe` (acknowledges the dynamic boundary)
unsafe:
    val data = some_untyped_lib.fetch()
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
    val port: int? = parse_port(os.environ.get("PORT"))
    var host: str = "localhost"

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
5. `host` is `var` because... well, in this example it isn't reassigned, so we could have used `val`. The compiler won't complain about over-mutability, but readers will.

## What you've learned

- **`val`** is for bindings you don't intend to rebind; **`var`** opts into mutability.
- **`T`** never holds `None`; **`T?`** does, and the checker tracks it.
- **Flow narrowing** lets you use `T?` values safely once you've checked them — `if`, `is None`, `isinstance`, `guard`.
- **No implicit `Any`** — either annotate or wrap in `unsafe`.

Next: [Functions](03-functions.md) — signatures, parameters, return types, and the rules around them.
