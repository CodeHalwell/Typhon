# tyc::non_exhaustive_match

Fires when a `match` on a value typed as a sealed union does not cover every
variant and does not have a wildcard arm.

## Example

```ty
type Shape = Circle | Square | Triangle

class Circle:
    radius: float
class Square:
    side: float
class Triangle:
    base: float
    height: float

def area(s: Shape) -> float:
    match s:
        case Circle(radius):
            return 3.14159 * radius * radius
        case Square(side):
            return side * side
    # error: missing `Triangle`
```

## Other closed subjects

The same check runs on every subject whose values form a closed set:

- **Nested sealed unions.** With `type Poly = Rect | Tri` and
  `type Shape = Circle | Poly`, a `match` over a `Shape` must cover the
  leaf classes `Circle`, `Rect` and `Tri` (`case Poly():` is not a class
  pattern: see `tyc::alias_not_a_class`).
- **`Result[T, E]`.** Both `Ok` and `Err` must be matched. When `E` (or `T`)
  is a sealed union, enum, `bool` or literal union, the payload patterns
  must cover it too: `case Err(NotFound())` and `case Err(Timeout())` over
  `Result[str, NotFound | Timeout | Denied]` report `Err(Denied)` missing.
- **Nullable subjects.** A `match` over `T?` must handle `None` once its
  arms cover `T` (`case int():` alone over an `int?` reports `None`).
- **`bool` and literal unions.** `case True:` alone reports `False`;
  `type Color = "red" | "green" | "blue"` reports any colour left out.

An arm with a guard (`case Ok(v) if v > 0:`) never counts towards coverage.
`[strictness] exhaustive-match` sets the level of every form.

The message names the subject by what it is: `on sealed union \`Shape\`:
missing variant(s) Triangle`, `on enum \`Color\`: missing member(s) BLUE`,
and, for the other closed subjects, the subject's type —
`on \`bool\`: missing case(s) False`,
`on \`int | None\`: missing case(s) None`,
`on \`Result[str, NotFound | Timeout | Denied]\`: missing case(s) Err(Denied)`.

## Why

Sealed unions list their variants exhaustively, which lets the type checker
prove (or refute) that a `match` handles every case.

## Fix

Either handle the missing variant explicitly, or add a `case _:` wildcard
arm if a default really is the right behaviour:

```ty
def area(s: Shape) -> float:
    match s:
        case Circle(radius):
            return 3.14159 * radius * radius
        case Square(side):
            return side * side
        case Triangle(base, height):
            return 0.5 * base * height
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/non_exhaustive_match.md
