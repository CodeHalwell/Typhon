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

### `frozen` + inheritance — ordering matters

When combining `frozen` with a base class, the modifier comes between the class name and the parenthesised base list, *not* after it:

```python
class Square frozen(Shape):       # ✅ parses
    side: float

class Square(Shape) frozen:       # ❌ does not parse
    side: float
```

The same rule applies to generics — type parameters sit before `frozen`: `class Stack[T] frozen:`, `class Stack[T] frozen(BaseStack):`.

## Mutable defaults are per-instance factories

`name: list[str] = []` (and the equivalent `{}` / `set()` / `list()` / `dict()` shapes) is *not* a Python pitfall in Typhon. The desugar pass rewrites mutable-literal defaults to `dataclasses.field(default_factory=...)`, so each instance gets its own fresh container instead of sharing one mutable literal:

```python
class Bucket:
    items: list[str] = []
    tags: dict[str, int] = {}

let a: Bucket = Bucket()
let b: Bucket = Bucket()
a.items.append("x")
print(b.items)         # [] — not shared
```

Since v0.9.0 the in-process VM (`tyc run`) also honours the factory; it previously executed the rewritten code path correctly under `tyc build` but shared one container across every instance under the VM. `tyc::class_attr_shadows_slot` correspondingly does *not* fire on mutable-literal defaults — those become per-instance fields, not class-level slot descriptors. The warning still fires on immutable literals (`int = 3`, `str = "x"`) where the slot-descriptor pitfall actually applies; annotate those as `ClassVar[T]`.

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

## `enum` — a fixed set of named members (v0.11.0)

When a type is one of a fixed set of named constants, reach for `enum` — it's sugar over `enum.Enum`, the same way `model` is sugar over `pydantic.BaseModel`:

```python
enum Direction:
    NORTH
    EAST
    SOUTH
    WEST

enum HttpStatus:
    OK = 200
    NOT_FOUND = 404
    SERVER_ERROR = 500
```

Bare members auto-number with `enum.auto()`; explicit `MEMBER = value` is preserved, and a subsequent bare member resumes `enum.auto()` counting from the last value — standard CPython `enum` semantics (e.g. `A = 10` then a bare `B` yields `B = 11`, not `2`). The emitted Python is exactly what you'd write by hand:

```python
import enum


class Direction(enum.Enum):
    NORTH = enum.auto()
    EAST = enum.auto()
    SOUTH = enum.auto()
    WEST = enum.auto()


class HttpStatus(enum.Enum):
    OK = 200
    NOT_FOUND = 404
    SERVER_ERROR = 500
```

`import enum` is injected for you, the form type-checks, and it runs under `tyc run` (the VM has a native `enum` shim). Add behaviour with an `impl` block just like any other class:

```python
impl HttpStatus:
    def is_error(self) -> bool:
        return self.value >= 400
```

> **`enum` vs sealed union.** Use `enum` for a fixed set of *valueless* (or simple-valued) named constants — days of the week, status codes, directions. Use a [sealed union](07-sealed-unions-and-match.md) when each variant carries its own *fields* (`Circle(radius)` vs `Rectangle(w, h)`). Sealed unions give you exhaustive `match` on the variant *shape*; enums give you a closed set of singletons.

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

> **`extend BUILTIN:` is module-local.** Unlike `extend ClassName:` (which merges into a user class and so crosses module boundaries), a built-in extension only rewrites call sites **inside the module that declares the `extend str:` block**. Importing that module does *not* carry the extension to the consumer — `title.slug()` in another module fires `tyc::attribute_not_found` on `str`, because the rewrite keys off the local block. This is deliberate: the rewrite is purely static (no runtime monkey-patch), so there is nothing for an `import` to bring along. When you need to share the behaviour across modules, wrap it in a plain free function and import *that*:
>
> ```python
> # textutil.ty
> pub def to_slug(s: str) -> str:
>     return s.lower().replace(" ", "-")
>
> # main.ty
> from textutil import to_slug
> let slug: str = to_slug(title)   # works across modules
> ```

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
