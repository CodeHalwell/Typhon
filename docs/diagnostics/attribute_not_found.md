# tyc::attribute_not_found

Fires when an attribute is accessed on a value whose static type doesn't
declare that attribute (and isn't `Any`).

## Example

```ty
class Point:
    x: int
    y: int

def main() -> None:
    let p: Point = Point(x=1, y=2)
    print(p.z)  # error: attribute `z` is not defined on `Point`
```

## Why

A misspelled attribute would otherwise propagate to runtime as
`AttributeError`. Resolving every attribute against the receiver's declared
type catches the typo at check time and points at the access site.

## Fix

Check the type's definition and use the correct attribute name, or add the
missing field if it should exist:

```ty
class Point:
    x: int
    y: int
    z: int
```


## Writes to undeclared attributes

A plain `class` compiles to `@dataclass(slots=True)`, so assigning an attribute
the class does not declare raises `AttributeError` at runtime — the object has
no `__dict__` to put it in. Since the 2026-09-30 review the write is reported
here instead of being read as a declaration (which also hid every later read
of the phantom attribute):

```ty
class P:
    x: int

impl P:
    def bump(self) -> None:
        self.y = 5      # error: attribute `y` is not defined on `P`
```

Declare the field, or use a `plain class` / `class!` (no slots) when the
class genuinely carries dynamic attributes. Classes with an unknown base or a
`__setattr__` / `__getattr__` are left alone.

## Unions

Accessing a member on a union reports the first member that lacks it — the
access is a crash whenever the value is that member — whether the union is a
`type` alias (`type U = A | B`), a parameter annotation, or a builtin pair
(`int | str`). A member every variant declares is fine:

```ty
class A:
    n: int
class B:
    s: str
type U = A | B

def f(u: U) -> int:
    return u.n          # error: attribute `n` is not defined on `B`
```

Narrow with `isinstance` or a `match` first.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/attribute_not_found.md
