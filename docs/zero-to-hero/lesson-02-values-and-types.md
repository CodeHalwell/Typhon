# Lesson 2 — Values and types

*Zero to Hero · Lesson 2 of 10*

This lesson covers Rules 2 and 3: how bindings work, and why a `str` is never `None`.

## `let` vs `mut`

These govern **binding** immutability — like Rust's `let`/`let mut` or TypeScript's `const`/`let`:

```python
def demo() -> None:
    let pi: float = 3.14159     # immutable binding
    mut counter: int = 0        # mutable binding
    counter = counter + 1       # ✅ reassign a mut
    # pi = 3.14                 # ❌ tyc::immutable_assign
```

**Reach for `let` first.** Switch to `mut` only when you actually rebind. A bare `name = "x"` with no keyword inside a function is `tyc::missing_binding_kind`.

`let` locks the *name*, not the *value* — `let u: User` still allows `u.name = "x"` if the field is mutable. (For deep immutability you'll meet `class … frozen:` in Lesson 4 and `freeze let` in Lesson 8.)

Module-level top bindings default to `let` if you skip the keyword; inside functions the keyword is always explicit.

## Primitives and widening

The primitive types are Python's: `int` (arbitrary precision), `float`, `bool`, `str`, `bytes`, `None`.

- `int` widens to `float` — `let y: float = 3` is fine.
- `float` does **not** narrow to `int` — write `int(x)` or `round(x)`.
- `bool` is a subtype of `int` — `let x: int = True` checks; `1 + True` is `int`. The reverse (`let b: bool = 1`) is rejected.

## Non-nullable by default

This is the rule that eliminates the billion-dollar mistake. A `str` is *never* `None`. If a value might be absent, its type is `str?` — sugar for `str | None`, and emitted exactly that way:

```python
def find(id: int) -> str?:      # may return None
    ...

def greet(name: str) -> None:   # never accepts None
    print(f"Hi {name}")

let found: str? = find(1)
# greet(found)                  # ❌ tyc::nullable_use — found might be None
```

## Narrowing

To use a `T?` as a `T`, you must *narrow* it — prove it isn't `None` on this path. The checker understands many forms:

```python
if found is not None:
    greet(found)                # ✅ narrowed to str inside the branch
```

Other narrowing forms: `is None`, `isinstance(x, T)`, early-return (`if x is None: return`), ternaries (`x if x is not None else default`), `and`/`or` short-circuits, exhaustive `match`, and the `guard` statement:

```python
guard f = find(1) else: return  # bail if None…
greet(f)                        # …so f is str from here on
```

`guard NAME = EXPR else: <diverge>` binds `NAME` to the non-`None` value, or runs the `else` block (which must `return`/`raise`/`continue`/`break`).

## Common mistakes

**`dict.get` is nullable.** `dict.get(k)` returns `V?`, not `V` — a frequent surprise:

```python
let scores: dict[str, int] = {"a": 1}
let s: int = scores.get("a")        # ❌ tyc::nullable_use — get returns int?
let s2: int = scores["a"]           # ✅ typed int (may raise KeyError)
```

**Re-binding with `let`.** You can't shadow a binding with another `let`; use `mut`, or pick a fresh name.

## Try it

1. Write `def initial(name: str?) -> str:` that returns the first character of `name`, or `"?"` if `name` is `None`. Use a `guard`.
2. Run `tyc check` — then try removing the `guard` and passing `name[0]` directly. Read the `tyc::nullable_use` diagnostic.
3. Add `let n: int = 5` then `n = 6` and watch `tyc::immutable_assign` fire. Change `let` to `mut` to fix it.

## What you learned

- `let` (immutable) vs `mut` (mutable) bindings — prefer `let`.
- Types are non-nullable; `T?` is the opt-in nullable form.
- Narrowing (`is not None`, `guard`, …) turns a `T?` into a usable `T`.

**Next:** [Lesson 3 — Functions, collections, and control flow](lesson-03-functions-and-control-flow.md).
