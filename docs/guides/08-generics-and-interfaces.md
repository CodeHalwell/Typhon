# 8. Generics and interfaces

Two tools for writing code that works across many types: **generics** (one definition, many parameter types) and **interfaces** (structural contracts — anything with the right shape satisfies them). Both erase at emit time — Python's duck typing carries the runtime; Typhon checks at compile time.

## Generic functions

Typhon uses **PEP 695** generic syntax: type parameters live in square brackets right after the name.

```python
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]
```

`T` is a type parameter scoped to this function. Call sites infer it from the arguments:

```python
let n: int? = first([1, 2, 3])           # T = int
let s: str? = first(["a", "b"])          # T = str
let none_int: int? = first([])           # ⚠️ T unconstrained — annotate the call:
let none_int: int? = first[int]([])      # ✅ explicit
```

When the argument is empty (or any case where `T` can't be inferred), spell `T` explicitly with `first[int](...)`.

### How inference works

Inference is **bidirectional**:

- Forward: parameter type → typevar binding. `first([1, 2, 3])` binds `T` to `int` because the list element is `int`.
- Recursive: structural patterns work. `def get[K, V](d: dict[K, V], k: K) -> V?` binds both `K` and `V` from the dict argument.
- Conflict resolution: if two parameters force conflicting bindings, the result widens to a union. `pair[T](a: T, b: T)` called as `pair(1, "two")` infers `T = int | str`.

Multi-argument constraint solving and bounded type vars are still partial in the current release — when in doubt, spell `T` explicitly.

## Generic classes

```python
class Box[T]:
    value: T

impl[T] Box[T]:
    def get() -> T:
        return value

    def map[U](f: Callable[[T], U]) -> Box[U]:
        return Box(value=f(value))
```

Use them like any other type:

```python
let b: Box[int] = Box(value=10)
let s: Box[str] = b.map(lambda n: f"n={n}")    # Box[str]
```

A few notes:

- `Box[T]` in the `class` declaration introduces `T`. Reference `Box[T]` (the concrete-but-still-generic type) inside `impl[T] Box[T]:`.
- `map` introduces a *new* type parameter `U` because it's converting `Box[T]` to `Box[U]`. The compiler binds `U` from the return type of `f`.

## Type aliases

```python
type Vec[T] = list[T]
type Pair[A, B] = tuple[A, B]
type Lookup[K, V] = dict[K, list[V]]
```

Aliases are transparent — `Vec[int]` *is* `list[int]`. They're a readability tool, not a new type.

## Interfaces

An `interface` is a *structural* contract: any type that provides the listed members (with compatible signatures) satisfies the interface, no explicit `implements` clause required.

```python
interface Drawable:
    def draw() -> None
    def width() -> float
    def height() -> float

class Button:
    label: str

impl Button:
    def draw() -> None:
        print(f"[ {label} ]")
    def width() -> float:
        return float(len(label) + 4)
    def height() -> float:
        return 1.0

def render(d: Drawable) -> None:
    d.draw()

render(Button(label="click me"))    # ✅ Button satisfies Drawable
```

The checker verifies, at the call site, that `Button` provides `draw`, `width`, and `height` with compatible signatures. Nothing in `Button`'s declaration mentions `Drawable` — that's what "structural" means.

### Interfaces emit as `typing.Protocol`

```python
# Typhon
interface Drawable:
    def draw() -> None
    def width() -> float
    def height() -> float

# Emitted Python
from typing import Protocol

class Drawable(Protocol):
    def draw(self) -> None: ...
    def width(self) -> float: ...
    def height(self) -> float: ...
```

Mypy, pyright, and IDEs all understand `Protocol` the same way Typhon does.

### Runtime `isinstance` is rejected by default

This is a subtle but important rule. Python's `@runtime_checkable` validates only *attribute presence*, not signatures — so `isinstance(x, Drawable)` would say `True` for a class with a `draw` field that isn't even callable. Typhon refuses to compile bare `isinstance` against an interface:

```python
def render_if_able(x: object) -> None:
    if isinstance(x, Drawable):    # ❌ tyc::interface_isinstance
        x.draw()
```

```
error[tyc::interface_isinstance]: runtime `isinstance` against an interface is unsafe
                                  by default; use static narrowing or opt into
                                  `@runtime_checkable` explicitly
```

Either redesign to use static types (sealed union, generic parameter), or, if you genuinely need a runtime check, write an explicit predicate function.

### When to reach for an interface

- You have **multiple unrelated** types that should share behaviour. A sealed union doesn't fit (the variants aren't fixed); inheritance doesn't fit (the types are unrelated).
- The set of conforming types is **open** — third-party callers can write their own implementations.

For closed sets of variants, prefer a sealed union ([guide 7](07-sealed-unions-and-match.md)).

## Bounded type parameters (partial)

A bound says "any `T` *that satisfies* some interface or class":

```python
interface Ordered:
    def __lt__(other: Self) -> bool

def smallest[T: Ordered](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    mut best: T = xs[0]
    for x in xs[1:]:
        if x < best:
            best = x
    return best
```

Bounded type vars are listed as **partial** in the current implementation — the syntax parses, but full constraint solving across multiple arguments is still landing. For now, use bounds in single-argument signatures and verify your call sites with `tyc check`.

## Read-view covariance (v0.9.0)

Functions that only *read* from a collection should declare the read-view type rather than the concrete container. The checker covariates the type parameter for the standard read-only protocols, so passing a `list[Dog]` where a `Sequence[Animal]` is expected works without any cast:

```python
class Animal: name: str
class Dog(Animal): breed: str

def names(animals: Sequence[Animal]) -> list[str]:
    return [a.name for a in animals]

let dogs: list[Dog] = [Dog(name="rex", breed="poodle")]
names(dogs)                       # ✅ since v0.9.0
```

| Read-only protocol | Variance | Concrete types that flow in |
|---|---|---|
| `Sequence[T]` | covariant on `T` | `list[T]`, `tuple[T, ...]` |
| `Iterable[T]` | covariant on `T` | `list[T]`, `tuple[T, ...]`, `set[T]`, `frozenset[T]`, generators |
| `Iterator[T]` | covariant on `T` | generators, `iter(...)` results |
| `Collection[T]` | covariant on `T` | `list[T]`, `tuple[T, ...]`, `set[T]`, `frozenset[T]` |
| `Container[T]` | covariant on `T` | every collection with `__contains__` |
| `Reversible[T]` | covariant on `T` | `list[T]`, `tuple[T, ...]` |
| `Mapping[K, V]` | K invariant, V covariant | `dict[K, V]`, `MappingProxyType[K, V]` |
| `MutableMapping[K, V]` | K invariant, V covariant | `dict[K, V]` |

Use the read-view spelling when a function does not mutate — it accepts more callers without losing precision. Reserve `list[T]` / `dict[K, V]` for functions that mutate:

```python
def total(xs: Sequence[float]) -> float:        # ✅ accepts list[float], tuple[float, ...]
    return sum(xs)

def append_zero(xs: list[float]) -> None:        # mutates — list[float] required
    xs.append(0.0)
```

`list[Dog]` does *not* flow into `list[Animal]` — writes through the wide view would let you `append(Animal())` to a `list[Dog]`. The covariance is read-protocol-only.

## Explicit type instantiation is not supported (v0.9.0)

Type parameters are always inferred from arguments or the binding type:

```python
let xs = first[int]([])              # ❌ check-time error since v0.9.0
let xs: int? = first([])             # ✅ T inferred as int from the binding
```

`func[T](args)` is rejected at check time with `tyc::operator_type_mismatch`. Before v0.9.0 this crashed at runtime with `'function' object is not subscriptable`. The fix is always to annotate the binding (forward inference) or the result position (backward inference). Generic *class* construction (`Box[int](value=7)`) is not affected — the `[int]` there is part of the class shape, not function-application syntax.

## Higher-kinded type parameters (parser scaffold)

A **class** type parameter can declare itself as a *type constructor* (a generic that takes a type argument) via the `[_]` marker:

```python
from typing import Callable

class Functor[F[_]]:
    pass

impl[F] Functor[F]:
    def map_through[A, B](self, fa: F[A], f: Callable[[A], B]) -> F[B]:
        return f(fa)
```

The parser accepts `F[_]` as a 1-arg type-constructor parameter on a **`class` header**, and since v1.0.0-alpha the checker unifies `F` against a concrete head (`F[A]` against `list[int]` binds `F = list, A = int`), reporting `tyc::kind_mismatch` on a wrong arity or a conflicting binding.

**Function-level and `interface`-level HKT parameters do not parse yet.** Both of these are a hard `tyc::parse` error today:

```python
def map_through[F[_], A, B](fa: F[A], f: Callable[[A], B]) -> F[B]:   # ❌ tyc::parse
    ...

interface Functor[F[_]]:                                              # ❌ tyc::parse
    def map[A, B](self, fa: F[A], f: Callable[[A], B]) -> F[B]: ...
```

Declare the constructor variable on a `class` and hang the methods off an `impl[F]` block, as above. The remaining tail is tracked in [`TYPE_SYSTEM_FRONTIER.md`](../../TYPE_SYSTEM_FRONTIER.md).

## Generics emit as type-erased Python

Typhon erases generics at emit time. The runtime sees plain `list`, `dict`, and untyped classes; type safety is a compile-time property only.

```python
# Typhon
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]

# Emitted Python
def first[T](xs: list[T]) -> T | None:
    if len(xs) == 0:
        return None
    return xs[0]
```

PEP 695 syntax is preserved (Python 3.12+ supports it natively). Older targets would need a `TypeVar` rewrite — for 3.13+ (Typhon's default), the syntax goes through unchanged.

## Putting it together

A small worked example: a generic in-memory cache with a structural "serialisable" interface.

```python
interface Serialisable:
    def to_dict() -> dict[str, str]

class Cache[K, V]:
    store: dict[K, V]

impl[K, V] Cache[K, V]:
    def get(k: K) -> V?:
        return store.get(k)

    def put(k: K, v: V) -> None:
        store[k] = v

def snapshot[T: Serialisable](items: list[T]) -> list[dict[str, str]]:
    return [item.to_dict() for item in items]

class User:
    id: int
    name: str

impl User:
    def to_dict() -> dict[str, str]:
        return {"id": str(id), "name": name}

def main() -> None:
    let users: Cache[int, User] = Cache(store={})
    users.put(1, User(id=1, name="Alice"))
    users.put(2, User(id=2, name="Bob"))

    let all_users: list[User] = list(users.store.values())
    let dump: list[dict[str, str]] = snapshot(all_users)
    print(dump)
```

What's going on:

- `Cache[K, V]` is generic over both key and value types. `Cache[int, User]` is a concrete instantiation.
- `Serialisable` is an interface — `User` satisfies it because it has `to_dict() -> dict[str, str]`. There's no `implements Serialisable` clause.
- `snapshot[T: Serialisable](...)` constrains `T` to "anything with `to_dict`". The compiler verifies `User` qualifies.

## Common mistakes

**Using `TypeVar` from `typing`:**

```python
from typing import TypeVar
T = TypeVar("T")

def first(xs: list[T]) -> T | None: ...    # ❌ use PEP 695
```

Fix: `def first[T](xs: list[T]) -> T?:`. Typhon does not use the `TypeVar` import path.

**Calling `isinstance` on an interface:**

See above — use static narrowing, or refactor to a sealed union if the variants are closed.

**Unconstrained inference:**

```python
let empty = first([])    # ❌ T unconstrained, can't infer
```

Annotate explicitly: `let empty: int? = first[int]([])`.

**Mixing structural and nominal expectations:**

```python
interface HasName:
    def name() -> str

class Box:
    name: str    # field, not method

def show(x: HasName) -> None: ...
show(Box(name="b"))    # ❌ Box.name is a field; HasName expects a method
```

Either change `HasName.name()` to a field, or change `Box.name` to a method (via `impl`).

## What you've learned

- Generic functions and classes use PEP 695 (`def f[T](...)`, `class Box[T]:`, `type Vec[T] = list[T]`).
- Inference is bidirectional; annotate when it can't pin `T` down.
- Interfaces are structural — anything with the right members satisfies them; emission is `typing.Protocol`.
- Runtime `isinstance` against interfaces is rejected; use static narrowing or sealed unions.
- Generics are erased at emit; the runtime is plain Python.

Next: [Async and concurrency](09-async-and-concurrency.md) — `async`/`await`, `gather:` blocks, and `go` spawn.
