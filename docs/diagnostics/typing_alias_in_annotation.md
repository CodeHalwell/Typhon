# tyc::typing_alias_in_annotation

Advisory lint (warning). Fires when a deprecated capitalised `typing`
alias (`List`, `Dict`, `Tuple`, `Set`, `FrozenSet`, `Type`, …) is used in
an annotation. Typhon targets CPython 3.13+, where the lowercase built-in
generics (`list[int]`, `dict[str, int]`, …) are the idiomatic form and the
`typing` aliases are deprecated.

## Example

```ty
# tyc::typing_alias_in_annotation — use the built-in generic
def f(xs: List[int]) -> Dict[str, int]: ...

# ✅ lowercase built-ins
def f(xs: list[int]) -> dict[str, int]: ...
```

## Fix

Use the lowercase built-in: `List` → `list`, `Dict` → `dict`, `Tuple` →
`tuple`, `Set` → `set`, `FrozenSet` → `frozenset`, `Type` → `type`.

Importing these names via `from typing import List` is rejected outright
with `tyc::typing_alias_deprecated`; this lint covers the annotation-site
use.

## See also

- `tyc::typing_alias_deprecated` — importing the deprecated alias.
- `tyc::typevar_import_rejected` — `from typing import TypeVar` (use PEP 695).
