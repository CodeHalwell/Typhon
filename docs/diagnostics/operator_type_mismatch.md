# tyc::operator_type_mismatch

Fires when a binary operator is applied to operands whose types are
incompatible per Python's runtime semantics — for example `str + int` or
`list + dict`. The check is conservative: it only fires on clearly-wrong
pairs whose types are both fully known and neither side is a user-defined
class (which might define its own `__add__` / `__mul__` / etc.).

## Example

```ty
def main() -> None:
    let result: str = "x" + 1  # error: unsupported operand types for `+`
```

## Why

Python would raise `TypeError: can only concatenate str (not "int") to str`
at runtime. Catching obviously-wrong combinations at check time avoids the
runtime error and points at the operator directly.

## Fix

Convert one operand so the types match:

```ty
def main() -> None:
    let result: str = "x" + str(1)
```

See https://typhon.dev/lang/diagnostics/operator_type_mismatch
