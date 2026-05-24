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

## Higher-kinded type parameters

A **higher-kinded type parameter** (HKT) is a type parameter that is itself a *type constructor* — a parameterised type that takes another type to produce a concrete type. The Typhon syntax for this is `F[_]`:

```python
class Functor[F[_]]:
    pass

impl[F[_]] Functor[F[_]]:
    def map[A, B](fa: F[A], f: Callable[[A], B]) -> F[B]:
        raise NotImplementedError
```

The `F[_]` notation says "`F` is not a plain type, it is a one-argument type constructor (like `list`, `set`, or any generic class you define)." You can then apply it in method signatures — `F[A]` and `F[B]` — and the checker infers which concrete constructor `F` stands for at each call site.

### How it works

When the checker sees `F[A]` in a method signature with `A` a known TypeVar and `F` a single uppercase letter (the universal TypeVar naming convention for constructors), it recognises `F[A]` as a higher-kinded application. At a call site, binding `F[A]` against `list[int]` sets `F → list` and `A → int`, which is then substituted into the return type:

```python
def map_list[A, B](fa: list[A], f: Callable[[A], B]) -> list[B]:
    return [f(x) for x in fa]

# For a HKT-generic version, F is inferred from the concrete argument:
let doubled: list[str] = map(some_list_int, str)    # F = list, A = int, B = str
```

### Example: `Functor` pattern

```python
interface Mappable[F[_]]:
    def fmap[A, B](fa: F[A], f: Callable[[A], B]) -> F[B]

class ListFunctor:
    pass

impl ListFunctor:
    def fmap[A, B](fa: list[A], f: Callable[[A], B]) -> list[B]:
        return [f(x) for x in fa]
```

### Current limitations

- HKT inference is **one-level deep** in v0.5.1: `F[A]` patterns are recognised when `F` is a single uppercase letter. Multi-letter constructor TypeVars (e.g. `M[_]`) require explicit `TypeConstructor` form via the `F[_]` class-param syntax.
- Constraint solving across multiple HKT parameters in a single call is partial — the checker binds what it can and falls back to `Unknown` for unresolvable positions. Annotate explicit type arguments when the checker can't infer.

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

## Generic variance

**Variance** describes how subtyping of a generic type `G[T]` relates to subtyping of `T`. Typhon infers variance automatically for user-defined generic classes — you do not need to annotate `TypeVar(covariant=True)`.

### Covariant — read-only producers

A type parameter is **covariant** when it only appears in *output* (return) positions. A `Reader[T]` that only reads values of type `T` is covariant: if `str` is a subtype of `object`, then `Reader[str]` is a subtype of `Reader[object]`.

```python
class Reader[T] frozen:
    value: T

impl[T] Reader[T]:
    def get(self) -> T:
        return self.value
```

Because `Reader` is frozen (fields are read-only) and `T` only appears in output positions, the checker infers `T` is **Covariant**. This means:

```python
let rs: Reader[str] = Reader(value="hello")
let ro: Reader[object] = rs    # ✅ Reader[str] is a Reader[object]
```

### Contravariant — write-only consumers

A type parameter is **contravariant** when it only appears in *input* (parameter) positions. A `Sink[T]` that only consumes values is contravariant: `Sink[object]` can safely stand in for `Sink[str]` because it accepts anything `str` does.

```python
class Sink[T]:
    pass

impl[T] Sink[T]:
    def push(self, value: T) -> None:
        print(value)
```

`T` only appears in parameter position → inferred **Contravariant**:

```python
let so: Sink[object] = Sink()
let ss: Sink[str] = so    # ✅ Sink[object] is a Sink[str] (contravariant)
```

### Invariant — both readable and writable

A type parameter is **invariant** when it appears in both input and output positions — the common case for a mutable container. A non-frozen `Box[T]` with a readable and assignable field is invariant: `Box[str]` is neither a subtype nor a supertype of `Box[int]`.

```python
class Box[T]:
    value: T    # non-frozen: both read and write → Invariant
```

This matches Python's own rule: `list[str]` is not a `list[object]` because you could append an `object` to the list through the wider type.

### How Typhon infers variance

At the end of class collection (after all `impl` blocks are merged), the checker walks each generic class's fields and method signatures:

| Position | Variance contributed |
|---|---|
| Field in a **non-frozen** class | Both covariant and contravariant → **Invariant** |
| Field in a **frozen** class | Covariant only (read-only) |
| Method **parameter** type | Contravariant |
| Method **return** type | Covariant |
| Type param not mentioned anywhere (phantom) | **Invariant** (conservative) |

When a parameter contributes both co- and contravariant positions the result is Invariant. Built-in Python generics (`list`, `dict`, `Sequence`, `Callable`, …) use a hand-maintained variance table; user-defined classes use this inference pass.

### Practical guidance

- **Immutable value containers** (`frozen class`): T is automatically Covariant — free upcasting works.
- **Event buses, callbacks, write channels** (only consume): T is automatically Contravariant.
- **Read-write containers** (`Box[T]`, `Cache[K, V]`): T is Invariant — no implicit subtyping between different instantiations.
- You never write `TypeVar("T", covariant=True)` in Typhon; the inference is automatic.

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
- **Higher-kinded type parameters** (`F[_]`) let you write generic code over type constructors, not just concrete types — the checker unifies `F[A]` against `list[int]` to bind `F → list`.
- Interfaces are structural — anything with the right members satisfies them; emission is `typing.Protocol`.
- Runtime `isinstance` against interfaces is rejected; use static narrowing or sealed unions.
- **Variance is inferred automatically**: frozen-field classes → Covariant; write-only method params → Contravariant; mutable fields or both positions → Invariant. No `TypeVar(covariant=True)` syntax needed.
- Generics are erased at emit; the runtime is plain Python.

Next: [Async and concurrency](09-async-and-concurrency.md) — `async`/`await`, `gather:` blocks, and `go` spawn.
