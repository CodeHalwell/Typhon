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

## Cross-newtype arithmetic

The same code also fires when two distinct `newtype`s with a shared
numeric base are combined arithmetically:

```ty
newtype LogIndex = int
newtype Term = int

def main() -> None:
    let idx: LogIndex = LogIndex(1)
    let t: Term = Term(5)
    let bad: LogIndex = idx + t   # ❌ operator `+` does not apply to `LogIndex` and `Term`
```

`LogIndex` and `Term` deliberately live on different axes — the runtime
values are both ints, but at the type level they should not be silently
interchangeable just because they share a base. Same-newtype arithmetic
(`LogIndex + LogIndex`) and one-sided arithmetic with a literal of the
newtype's base (`LogIndex + 1`) preserve the newtype; cross-newtype use
fires this diagnostic.

Wrap explicitly at the boundary if the cross is intentional:

```ty
let bumped: LogIndex = idx + LogIndex(int(t))
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/operator_type_mismatch.md
