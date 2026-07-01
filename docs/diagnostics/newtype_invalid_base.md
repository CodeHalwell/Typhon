# tyc::newtype_invalid_base

Fires when a `newtype` is declared over a base that is not a valid type —
for example a string or numeric literal instead of a type expression.
A `newtype` must wrap an existing type (`int`, `str`, another class, a
parametric container, …); it lowers to a zero-cost `typing.NewType` call,
which requires a real base type.

## Example

```ty
# tyc::newtype_invalid_base — the base must be a type, not a literal
newtype Color = "red"

# ✅ wrap an actual type
newtype UserId = int
newtype Tags = list[str]
```

## Fix

Give the `newtype` a type expression as its base. If you wanted a fixed
set of string values, use a string-literal union or an `enum` instead:

```ty
type Color = "red" | "green" | "blue"     # string-literal singleton union
```

## See also

- `tyc::newtype_violation` — a wrong-typed value flowing into a `newtype`.
