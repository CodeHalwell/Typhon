# Lesson 3 — Functions, collections, and control flow

*Zero to Hero · Lesson 3 of 10*

This lesson covers Rule 1 (annotate everything) in the small, plus the everyday machinery: functions, generics, collections, and loops.

## Functions

Annotate every parameter and the return type. There are no exceptions:

```python
def add(a: int, b: int) -> int:
    return a + b

def log(message: str) -> None:   # returns nothing → -> None
    print(message)
```

Defaults and keyword arguments work as in Python. `*args` / `**kwargs` work too — they just need annotations, and the idiom is `object`:

```python
def connect(host: str, port: int = 8080) -> str:
    return f"{host}:{port}"

def variadic(*args: object, **kwargs: object) -> None:
    ...
```

## Generics (a first taste)

Generics use PEP 695 syntax — and **only** that. Never `from typing import TypeVar`:

```python
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]
```

`first([1, 2, 3])` infers `T = int` and returns `int?`. Inference is bidirectional, so you rarely write type arguments by hand. (Lesson 7 goes deeper on generics.)

## Collections

Element types are required — a bare `list` is a missing-annotation error:

```python
let xs: list[int] = [1, 2, 3]
let scores: dict[str, int] = {"alice": 90}
let point: tuple[float, float] = (1.0, 2.0)
let nums: tuple[int, ...] = (1, 2, 3)        # variadic homogeneous tuple
```

Remember from Lesson 2: `dict.get(k)` returns `V?`. Use `d[k]` when you want the value typed `V` directly.

## Control flow

`if`/`elif`/`else`, `for`, `while`, and comprehensions are Python — with the binding rules applied. One important subtlety: loop, `with`, `case`, and `except` *targets* are **bindings, not assignments**, so they don't take `let`/`mut`:

```python
def total(rows: list[dict[str, int]]) -> int:
    mut sum: int = 0
    for row in rows:                 # `row` is a for-target — no keyword
        sum = sum + row["amount"]
    return sum
```

Comprehensions are first-class and just as type-checked:

```python
let evens: list[int] = [n for n in range(20) if n % 2 == 0]
let lengths: dict[str, int] = {s: len(s) for s in ["a", "bb", "ccc"]}
let unique: set[int] = {n % 3 for n in range(10)}
```

## Common mistakes

**Untyped empty collection:**

```python
let xs: list = []        # ❌ tyc::missing_annotation — needs an element type
let xs: list[int] = []   # ✅
```

**Literal division by zero** is caught at compile time:

```python
let bad = 10 / 0         # ❌ tyc::div_by_zero_literal
```

**Unmanaged resource** — opening a file without `with` warns:

```python
let f = open("data.txt")           # ⚠ tyc::resource_not_managed
with open("data.txt") as f:        # ✅
    ...
```

## Try it

1. Write `def histogram(words: list[str]) -> dict[str, int]:` that counts how many times each word appears. Use a `mut` dict and a `for` loop, or a comprehension over `set(words)`.
2. Write a generic `def last[T](xs: list[T]) -> T?:` returning the final element or `None`.
3. Try `let counts: dict = {}` and read the diagnostic. Add the element types to fix it.

## What you learned

- Every function annotates its parameters and return type.
- Generics are PEP 695 (`def f[T](...)`).
- Collections carry element types; loop/`with`/`case` targets are bindings, not `let`/`mut` assignments.

**Next:** [Lesson 4 — Classes, models, and enums](lesson-04-classes-models-enums.md).
