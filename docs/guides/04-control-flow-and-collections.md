# 4. Control flow and collections

Most of Typhon's day-to-day syntax — branches, loops, lists, dicts — is plain Python. This guide focuses on what's different: `guard`, the way `match` is treated (covered fully in [guide 7](07-sealed-unions-and-match.md)), and how collection types are annotated under non-nullable rules.

## `if`, `elif`, `else`

Unchanged from Python:

```python
def classify(score: int) -> str:
    if score >= 90:
        return "A"
    elif score >= 75:
        return "B"
    elif score >= 60:
        return "C"
    else:
        return "F"
```

A branch must either return, raise (in `unsafe` regions), or fall through to a converging `return`. The checker tracks reachability; missing returns are flagged:

```python
def classify(score: int) -> str:
    if score >= 60:
        return "pass"
    # ❌ tyc::missing_return: function exits without returning a str
```

## `while`

```python
def countdown(n: int) -> None:
    mut i: int = n
    while i > 0:
        print(i)
        i = i - 1
    print("liftoff")
```

The loop variable is `mut` because we reassign it. The checker would catch a `let` here.

## `for`

`for` iterates anything `Iterable`:

```python
def sum_all(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        total = total + x
    return total
```

For index-and-value pairs, use `enumerate`. For two parallel lists, use `zip`. The element type is inferred from the iterable:

```python
let names: list[str] = ["a", "b", "c"]
for i, name in enumerate(names):
    print(f"{i}: {name}")
```

## `break` and `continue`

Same semantics as Python. `break` and `continue` count as control-flow exits for reachability analysis.

## `guard` — early-return with narrowing

`guard` binds a value and short-circuits the falsy/`None` case:

```python
def shipping_cost(weight: float?) -> float:
    guard w = weight else:
        return 0.0
    return w * 1.25
```

Inside the `else:` block you must return, raise, or otherwise leave the enclosing function. After the `guard`, the name (`w`) is narrowed to its non-null form.

`guard` is sugar for:

```python
if weight is None:
    return 0.0
let w: float = weight
return w * 1.25
```

…but it reads better, and it pushes the failure case up front where it belongs.

You can chain guards:

```python
def open_session(token: str?, user_id: int?) -> str:
    guard t = token else: return "anonymous"
    guard u = user_id else: return "anonymous"
    return f"session({t}, {u})"
```

## `match` (preview)

`match` exists, but its power lands in [guide 7](07-sealed-unions-and-match.md) when you have a sealed union to match against. The basics:

```python
def describe(n: int) -> str:
    match n:
        case 0:
            return "zero"
        case 1 | 2 | 3:
            return "small"
        case _:
            return "many"
```

For *sealed unions*, the wildcard (`_`) becomes optional — the checker enforces exhaustiveness automatically. We'll get there in guide 7.

## Collections

### `list[T]`

Mutable, ordered. The element type is required:

```python
let primes: list[int] = [2, 3, 5, 7, 11]

mut bag: list[str] = []
bag.append("hello")
```

A heterogeneous literal is rejected unless the annotation is a union:

```python
let mixed: list[int] = [1, "two"]            # ❌ str not assignable to int
let mixed: list[int | str] = [1, "two"]      # ✅
```

### `dict[K, V]`

```python
let counts: dict[str, int] = {"apples": 3, "pears": 1}
let n: int? = counts.get("apples")    # `.get` returns V | None
```

`dict.get(key)` returns `V?` — there's no implicit `None`-stripping. Either check it, narrow it, or use `dict[key]` (which raises `KeyError` and is typed `V`).

### `set[T]`

```python
let seen: set[int] = {1, 2, 3}
let also_seen: set[int] = set()
```

### `tuple[...]`

Fixed-arity tuples spell the type of every slot:

```python
let point: tuple[float, float] = (1.0, 2.0)
let rgb: tuple[int, int, int] = (255, 128, 0)
```

Variable-length homogeneous tuples use `tuple[T, ...]`:

```python
def average(*nums: float) -> float:
    # nums has type tuple[float, ...]
    return sum(nums) / len(nums)
```

## Comprehensions

List, set, and dict comprehensions are unchanged:

```python
let nums: list[int] = [1, 2, 3, 4, 5]
let squares: list[int] = [n * n for n in nums]
let evens: set[int] = {n for n in nums if n % 2 == 0}
let by_value: dict[int, int] = {n: n * n for n in nums}
```

Generator expressions exist; their type is `Iterator[T]`.

## Iterating safely over optional collections

A common shape: a function returns `list[T]?` (e.g. "none if not found"), and you want to iterate the result. Narrow first:

```python
def first_letters(words: list[str]?) -> list[str]:
    guard ws = words else: return []
    return [w[0] for w in ws if len(w) > 0]
```

The `guard` narrows `words` to `list[str]`, so the comprehension type-checks. Without the guard, `for w in words` would be a "nullable use" error.

## Exception handling

`try`/`except` is available but **rarely the right tool** in Typhon — error handling is meant to flow through `Result[T, E]` (next guide). Treat `try` as the boundary between Python's exception world and Typhon's typed-error world:

```python
import json

def parse(raw: str) -> dict[str, int]?:
    try:
        return json.loads(raw)
    except (ValueError, TypeError):
        return None
```

You'll typically wrap untyped or third-party calls in a tiny `try` and lift their failures into `Result[T, E]` — covered next.

## Common mistakes

**Missing element type on a collection:**

```python
let xs: list = [1, 2, 3]      # ❌ bare `list` is an implicit Any element type
```

Fix: `let xs: list[int] = [1, 2, 3]`.

**Treating `dict.get(...)` as non-nullable:**

```python
let counts: dict[str, int] = {"a": 1}
let n: int = counts.get("missing")    # ❌ get() returns int | None
```

Fix: `let n: int? = counts.get("missing")`, then narrow.

**Reassigning a `let` loop accumulator:**

```python
def sum_all(xs: list[int]) -> int:
    let total: int = 0
    for x in xs:
        total = total + x     # ❌ total is `let`
    return total
```

Fix: `mut total: int = 0`.

## What you've learned

- Branches and loops are plain Python, with reachability tracked by the checker.
- `guard` is the idiomatic "fail fast on a `None`" form, with narrowing afterwards.
- Collection annotations are mandatory (`list[T]`, `dict[K, V]`, `set[T]`, `tuple[A, B, ...]`).
- `dict.get(k)` returns `V?`; narrow before use.
- Use `try`/`except` only at the boundary; `Result` is the in-language error type — covered next.

Next: [Classes and models](05-classes-and-models.md) — defining your own types, dataclass vs Pydantic emission, and `impl`/`extend` blocks.
