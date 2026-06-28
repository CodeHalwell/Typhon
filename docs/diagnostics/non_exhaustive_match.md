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

See https://typhon.dev/lang/diagnostics/non_exhaustive_match
