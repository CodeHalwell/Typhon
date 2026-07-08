# tyc::impl_unknown_class

Fires when an `impl NAME:` block targets a class that does not exist in the
current module. The methods would otherwise lower into a free-floating
`__typhon_impl_NAME` pseudo-class that the merge pass silently drops,
producing dead code.

## Example

```ty
impl Point:  # error: no class `Point` declared in this module
    def length(self) -> float:
        return 0.0
```

## Why

`impl` blocks are merged into their target class at desugar time. Without a
target the methods have nowhere to go, and silently dropping them would
hide the typo (or missing `class` declaration) until the missing method
trips a runtime `AttributeError` elsewhere.

## Fix

Declare the class first, or fix the name to match an existing class:

```ty
class Point:
    x: int
    y: int

impl Point:
    def length(self) -> float:
        return (self.x * self.x + self.y * self.y) ** 0.5
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/impl_unknown_class.md
