# 5. Classes and models

Typhon's `class` is deliberately minimal: no `__init__`, no explicit `self`. The compiler emits a `@dataclass(slots=True)` by default, or a Pydantic `BaseModel` if you use the `model` keyword. Methods live in `impl` blocks, Rust-style.

## A first class

```python
class User:
    id: int
    name: str = "anon"
    email: str?
```

Three fields. Two have explicit defaults (or implicit ones — `email: str?` defaults to `None`). One is required (`id`).

The constructor is generated for you:

```python
let u: User = User(id=1, name="Alice", email="alice@example.com")
let anon: User = User(id=2)               # name defaults to "anon", email to None
```

**Compiles to:**

```python
from dataclasses import dataclass

@dataclass(slots=True)
class User:
    id: int
    name: str = "anon"
    email: str | None = None
```

`slots=True` is the default — instances don't carry a per-object `__dict__`, which saves memory and catches typos (`u.emial = "x"` is an `AttributeError`).

## Adding methods with `impl`

Methods live in a separate `impl` block. Take an explicit `self` parameter and access fields as `self.NAME`:

```python
class User:
    id: int
    name: str
    email: str?

impl User:
    def display(self) -> str:
        return f"{self.name} <{self.email}>" if self.email is not None else self.name

    def is_admin(self) -> bool:
        return self.id == 0
```

> **History note.** Earlier drafts described an implicit-`self` form (`def display() -> str: return f"{name} (#{id})"`). That form was never implemented — the resolver doesn't know which class an impl-block method belongs to at name-resolution time, so bare `name` reads as "unknown name". Use the explicit `self.NAME` syntax shown above.

Calls look normal:

```python
let u: User = User(id=1, name="Alice", email="alice@example.com")
print(u.display())            # Alice <alice@example.com>
print(u.is_admin())           # False
```

The desugarer **merges the `impl` block into the class**:

```python
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

You can split `impl` blocks across files (e.g. keep `User` definition in `models.ty` and add domain methods from `auth.ty`). The desugarer collects them all.

### `@property`

Decorate a no-argument method with `@property` to expose it as an attribute:

```python
class Rect:
    w: float
    h: float

impl Rect:
    @property
    def area(self) -> float:
        return self.w * self.h

let r: Rect = Rect(w=3.0, h=4.0)
let a: float = r.area     # not r.area() — `area` types as `float`, not `() -> float`
```

The type checker resolves `r.area` to the property's return type, so the access reads as a plain attribute read. Underneath, this lowers to standard Python `@property` semantics.

## Why `impl` instead of methods-in-class?

Two reasons:

1. **Data and behaviour are separable.** A `class` declaration shows the shape; `impl` blocks show what you can *do* with it. New methods can be added without touching the data definition.
2. **Methods are explicit, fields are nominal.** Write `def display(self) -> str:` and access fields via `self.NAME` — the resolver can't see the enclosing class's fields from a method body otherwise. (If you forget `self` on a method that touches fields, you'll get `tyc::unknown_name` for each field reference.)

## Mutability of fields

Fields are mutable by default; the `frozen` modifier on the class (emitted as `frozen=True`) disables field reassignment:

```python
class Point:
    x: float
    y: float

let p: Point = Point(x=1.0, y=2.0)
p.x = 5.0    # ✅ allowed

class FrozenPoint frozen:
    x: float
    y: float

let q: FrozenPoint = FrozenPoint(x=1.0, y=2.0)
q.x = 5.0    # ❌ dataclasses.FrozenInstanceError at runtime; tyc::frozen_assign at compile time
```

> **Important caveat:** dataclass `frozen=True` only stops field reassignment. If a field is a mutable container (`list`, `dict`), the container's contents can still be mutated. For deeper guarantees, use immutable containers (`tuple`, `frozenset`) inside the class.

## `model` — Pydantic emission

When you need runtime validation — typical for API boundaries — use `model`:

```python
model ApiUser:
    id: int
    email: str
    name: str = "anon"
```

**Compiles to:**

```python
from pydantic import BaseModel, ConfigDict

class ApiUser(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: int
    email: str
    name: str = "anon"
```

Two things to notice:

- **`extra="forbid"`** is the default. Pydantic's stock setting is `"ignore"`, which silently drops unknown fields — exactly the kind of quiet failure Typhon exists to prevent. Override it project-wide in `typhon.toml`:

  ```toml
  [emit]
  model-extra = "allow"   # or "ignore"
  ```

- **Pydantic validates at construction time**, not at compile time. `ApiUser(id="oops", email="a@b")` raises `ValidationError` at runtime. The type checker catches the same thing at compile time, but `model` adds a second line of defence for data crossing trust boundaries (HTTP requests, file inputs).

### When to use `model` vs `class`

| Use `class` (dataclass) | Use `model` (Pydantic) |
|-------------------------|-------------------------|
| Internal types, fully under your control | Data from outside (HTTP, JSON files, env, queues) |
| You want maximum performance | You want runtime validation |
| You don't need extra constraints | You need `Field(min_length=1, ...)` style validators |

You can mix both freely in one project.

## `extend` — adding methods to existing classes

`extend` is `impl`'s twin for user-defined classes you don't own (or don't want to mix concerns with):

```python
# In domain/user.ty
class User:
    id: int
    name: str

# In analytics/user_metrics.ty
extend User:
    def tracking_id() -> str:
        return f"user-{id:08d}"
```

The merge happens at desugar; downstream callers see a single class with both sets of methods.

Built-in extensions (e.g. `extend str:`) are also supported: each method is extracted to a module-level free function `__typhon_ext_str__METHOD` at desugar time, and call sites like `"hi".to_slug()` are rewritten to `__typhon_ext_str__to_slug("hi")` whenever the receiver has a static `str` annotation. There is no monkey-patching of built-ins; un-annotated receivers fall back to native attribute lookup (i.e. `AttributeError` when the method does not exist on the underlying type).

## Inheritance

Single inheritance works the way it does in Python. You spell the parent class in parentheses:

```python
class Animal:
    name: str

impl Animal:
    def speak() -> str:
        return "..."

class Dog(Animal):
    breed: str

impl Dog:
    def speak() -> str:
        return "woof"
```

**Caveat:** dataclass inheritance has a wrinkle — fields without defaults must come before fields with defaults across the whole MRO. The checker will flag the conflict, but it's worth knowing the underlying constraint.

For most domain modelling, prefer **sealed unions** ([guide 7](07-sealed-unions-and-match.md)) over inheritance. They give you exhaustive matching, and the checker enforces variant coverage in a way subclassing can't.

## Putting it together

A worked example: a tiny user system that mixes a `model` (for the API), a `class` (for internal state), and `impl` blocks for methods.

```python
# src/users.ty
from datetime import datetime

model UserInput:
    email: str
    name: str = "anon"

class User:
    id: int
    email: str
    name: str
    created_at: datetime

impl User:
    def display() -> str:
        return f"{name} <{email}>"

    def age_seconds(now: datetime) -> float:
        return (now - created_at).total_seconds()

def create(input: UserInput, id: int) -> User:
    return User(
        id=id,
        email=input.email,
        name=input.name,
        created_at=datetime.now(),
    )
```

Notes:

- `UserInput` is a `model` because it comes from outside (an HTTP body, say). Pydantic will reject `{"email": "a@b", "name": "alice", "extra": "boom"}` at construction time.
- `User` is a `class` because it lives entirely inside the app — no validation needed, just a dataclass.
- `display` and `age_seconds` are methods on `User`, defined in a separate `impl` block. The compiler merges them in.
- `create` is a free function. There's no requirement that constructor-like functions live as static methods; module-level functions are first-class.

## Common mistakes

**Writing `__init__`:**

```python
class User:
    id: int

    def __init__(self, id: int) -> None:    # ❌
        self.id = id
```

```
error[tyc::manual_init]: classes do not declare `__init__`; the constructor is generated
```

Fix: drop the method. Use field defaults for "convenience" constructors, or write a free function (`def make_user(...) -> User: ...`).

**Putting methods inside `class`:**

```python
class User:
    id: int

    def display(self) -> str:      # ❌ wrong place
        return f"user {self.id}"
```

Fix: move into an `impl User:` block, drop `self`:

```python
class User:
    id: int

impl User:
    def display() -> str:
        return f"user {id}"
```

**Mutating a frozen instance:**

```python
class FrozenPoint frozen:
    x: float
    y: float

let p: FrozenPoint = FrozenPoint(x=1.0, y=2.0)
p.x = 3.0     # ❌ tyc::frozen_assign
```

Construct a fresh instance instead: `let q: FrozenPoint = FrozenPoint(x=3.0, y=p.y)`.

## What you've learned

- `class` emits `@dataclass(slots=True)`; `model` emits a Pydantic `BaseModel(extra="forbid")`.
- Methods live in `impl` blocks; `self` is implicit and inserted at desugar time.
- `extend` adds methods to existing user-defined classes from elsewhere.
- Prefer composition / sealed unions over inheritance for domain modelling.

Next: [Error handling with `Result`](06-error-handling.md) — typed errors, the `?` operator, and `with`-chains.
