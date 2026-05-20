# tyc::typing_alias_deprecated

Fires when a module imports a deprecated capitalised collection alias from
`typing` (`List`, `Dict`, `Tuple`, `Set`, `FrozenSet`, `Type`). Typhon
prefers the built-in lowercase forms for consistency with the rest of the
language.

## Example

```ty
from typing import List  # error: deprecated alias
def main() -> None:
    let xs: List[int] = [1, 2, 3]
```

## Why

Since PEP 585, every built-in container supports the `[T]` parameterisation
directly. The capitalised aliases were a transitional convenience and are
now redundant; using them creates two ways to write the same type, which
adds noise to imports and inconsistency across the codebase.

## Fix

Drop the import and use the lowercase built-in:

```ty
def main() -> None:
    let xs: list[int] = [1, 2, 3]
```

See https://typhon.dev/lang/diagnostics/typing_alias_deprecated
