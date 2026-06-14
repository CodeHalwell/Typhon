# Lesson 4 — Classes, models, and enums

*Zero to Hero · Lesson 4 of 10*

This lesson covers Rule 4 (methods live in `impl`) and the several class *forms*, each of which emits different Python. Pick by intent.

## The class forms

| Form | Emits | Use for |
|---|---|---|
| `class Foo:` | `@dataclass(slots=True)` | plain value types (the default) |
| `class Foo frozen:` | `@dataclass(slots=True, frozen=True)` | immutable value types |
| `model Foo:` | `class Foo(BaseModel)` + `extra="forbid"` | validated input at a system boundary (Pydantic) |
| `enum Foo:` | `class Foo(enum.Enum)` | a fixed set of named members |
| `interface Foo:` | `class Foo(Protocol)` | structural contracts (Lesson 7) |
| `plain class Foo:` | bare `class Foo:` | metaclass-driven / ORM libraries |
| `class! Foo(Base):` | bare `class Foo(Base):` + synthesised `__init__` | subclassing a framework base (e.g. `nn.Module`) |

## Plain value types

A `class` is a dataclass. There is **no `__init__`** — it's generated, and writing one is an error (`tyc::manual_init`):

```python
class User:
    id: int
    name: str

let alice: User = User(id=1, name="Alice")
print(alice.name)
```

Add `frozen` for immutability — field reassignment then becomes `tyc::frozen_assign`:

```python
class Point frozen:
    x: float
    y: float
```

## Methods live in `impl`

This is the biggest visual departure from Python. The `class` body holds **fields only**; methods go in a separate `impl` block, with an explicit `self`, accessing fields via `self.NAME`:

```python
class User:
    id: int
    name: str

impl User:
    def display(self) -> str:
        return f"{self.name} (#{self.id})"

    def with_name(self, name: str) -> User:
        return User(id=self.id, name=name)
```

`impl` and `class` can even live in different files. The sibling keyword `extend` does the same across modules (and `extend str:` / `extend list:` can add static methods to built-ins). At emit time, the methods are merged back into the class body — the generated Python is a perfectly normal class.

Why split them? It keeps the data shape declarative, makes cross-module extension uniform, and lets you attach methods to a *sealed union* and have them distributed to every variant (Lesson 6).

## `model` — validated boundaries

A `model` validates at construction — ideal for untrusted JSON, config, or request bodies. It's sugar over Pydantic's `BaseModel`:

```python
model ApiUser:
    id: int
    name: str
    email: str?              # optional field
```

## `enum` — fixed sets of members

`enum` is sugar over `enum.Enum`. Bare members auto-number; explicit values are preserved. From `examples/49-enums/enums.ty`:

```python
enum Direction:
    NORTH
    EAST
    SOUTH
    WEST

enum Priority:
    LOW = 10
    MEDIUM = 20
    HIGH = 30
    URGENT                   # resumes auto-numbering at 31
```

Members repr as `Direction.NORTH`, iterate in declaration order, and round-trip through the constructor (`Priority(30)` is `HIGH`). They're great `match` subjects:

```python
def sla_minutes(p: Priority) -> int:
    match p:
        case Priority.LOW:    return 24 * 60
        case Priority.MEDIUM: return 4 * 60
        case Priority.HIGH:   return 60
        case Priority.URGENT: return 15
    raise RuntimeError("unreachable")
```

## Common mistakes

**Writing `__init__`:**

```python
class User:
    id: int
    def __init__(self, id: int) -> None:   # ❌ tyc::manual_init
        self.id = id
```

Fix: delete it — the constructor is generated. Use field defaults if you need them.

**A method inside the `class` body:**

```python
class User:
    id: int
    def display(self) -> str: ...          # ⚠ tyc::method_in_class_body
```

Fix: move it into `impl User:`.

**Defaulted field before a non-defaulted one** (`@dataclass` would raise at import):

```python
class Cfg:
    timeout: int = 30
    host: str                              # ❌ tyc::field_default_ordering
```

Fix: put non-defaulted fields first.

## Try it

1. Define `class Rect frozen: width: float; height: float` and an `impl Rect:` with `def area(self) -> float:`.
2. Build it (`tyc build`) and read `build/main.py` — see the `@dataclass(slots=True, frozen=True)` and the merged method.
3. Define `enum TrafficLight: RED; AMBER; GREEN` and a function that `match`es it to a duration in seconds.

## What you learned

- The class forms (`class`, `frozen`, `model`, `enum`, `interface`, `plain class`, `class!`) and when to reach for each.
- Fields go in `class`; methods go in `impl` (or `extend`), with explicit `self`.
- The constructor is generated — never write `__init__`.

**Next:** [Lesson 5 — Error handling with `Result`](lesson-05-error-handling.md).
