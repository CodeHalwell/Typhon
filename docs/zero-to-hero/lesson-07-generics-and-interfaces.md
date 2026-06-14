# Lesson 7 — Generics and interfaces

*Zero to Hero · Lesson 7 of 10*

Two tools for writing code that works across many types: parametric generics, and structural interfaces.

## Generics (PEP 695)

Generics put type parameters in square brackets — on functions, classes, and aliases. This is the *only* generic syntax Typhon accepts; never `from typing import TypeVar`.

```python
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]


class Box[T]:
    value: T

impl[T] Box[T]:
    def map[U](self, f: Callable[[T], U]) -> Box[U]:
        return Box(value=f(self.value))


type Pair[A, B] = tuple[A, B]
```

Notes:

- `impl[T] Box[T]:` introduces the type parameter for the methods; an individual method can add its own (`def map[U]`).
- Inference is **bidirectional and recursive** — `Box(value=3).map(lambda n: n > 0)` infers `T = int`, then `U = bool`, giving `Box[bool]`. You almost never write the type arguments yourself.
- Generics erase at emit time; the PEP 695 syntax is preserved for stock Python 3.13+.

## Interfaces (structural)

An `interface` is a structural contract — a Python `Protocol` under the hood. A type conforms simply by having the right methods; no `implements`, no base class, no registration:

```python
interface Drawable:
    def draw(self) -> None
    def width(self) -> float


class Button:
    label: str

impl Button:
    def draw(self) -> None:
        print(self.label)
    def width(self) -> float:
        return float(len(self.label) + 4)


def render(d: Drawable) -> None:
    d.draw()

render(Button(label="x"))        # ✅ Button structurally matches Drawable
```

Conformance is by *signature*, not just method name — parameter and return types must match too.

## Interfaces vs sealed unions

Both let one function handle many types, but they pull in opposite directions:

| | `interface` | sealed union |
|---|---|---|
| Membership | **Open** — any type with the right methods | **Closed** — a fixed variant list |
| Dispatch | Call methods on the contract | `match` on the variants |
| Add a type later | Just implement the methods | Edit the alias (and update matches) |

Use an interface when you want callers to plug in their own conforming types; use a sealed union (Lesson 6) when the set is fixed and you switch on it.

## Common mistakes

**`isinstance` against an interface is rejected:**

```python
if isinstance(x, Drawable):      # ❌ tyc::interface_isinstance
    ...
```

Python's runtime protocol check only verifies attribute *presence*, not signatures, so it's unsound. Narrow statically, or refactor to a sealed union and `match`.

**Reaching for `typing.TypeVar`:**

```python
from typing import TypeVar       # ❌ tyc::typevar_import_rejected
T = TypeVar("T")
```

Fix: use PEP 695 — `def f[T](...)`. (Likewise `from typing import List/Dict/...` is rejected — use the lowercase built-ins.)

## Try it

1. Write a generic `def swap[A, B](pair: tuple[A, B]) -> tuple[B, A]:`.
2. Define `interface Named: def name(self) -> str` and two unrelated classes that conform. Write `def greet(n: Named) -> str:` and call it with both.
3. Try `isinstance(x, Named)` and read the diagnostic.

## What you learned

- PEP 695 generics on functions, classes, and aliases; bidirectional inference fills in the type arguments.
- Structural `interface`s let any conforming type satisfy a contract.
- Interfaces are open; sealed unions are closed — choose by whether the set of types is fixed.

**Next:** [Lesson 8 — Domain modelling](lesson-08-domain-modelling.md).
