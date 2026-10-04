# tyc::alias_not_a_class

Fires when a `type` alias is used where Python needs a class: as a `match`
class pattern (`case Poly():`) or as the second argument of `isinstance`.

## Example

```ty
class Circle frozen:
    r: float
class Rect frozen:
    w: float
class Tri frozen:
    b: float
type Poly = Rect | Tri
type Shape = Circle | Poly

def area(s: Shape) -> float:
    match s:
        case Circle(r=r):
            return r
        case Poly():            # error: `Poly` is a type alias, not a class
            return 1.0

def is_poly(s: Shape) -> bool:
    return isinstance(s, Poly)  # error: same
```

## Why

`type Poly = Rect | Tri` emits a PEP 695 `type` statement, which creates a
`typing.TypeAliasType` object, not a class. CPython raises `TypeError`
("called match pattern must be a class" / "isinstance() arg 2 must be a
type, a tuple of types, or a union") as soon as such a pattern or call runs.

## Fix

Match or test the alias's variants instead:

```ty
    match s:
        case Circle(r=r):
            return r
        case Rect() | Tri():
            return 1.0

def is_poly(s: Shape) -> bool:
    return isinstance(s, (Rect, Tri))
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/alias_not_a_class.md
