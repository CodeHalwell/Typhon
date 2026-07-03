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

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/attribute_not_found.md
