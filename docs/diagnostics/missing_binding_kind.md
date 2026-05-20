# tyc::missing_binding_kind

Fires when a local assignment inside a function body introduces a new name
without a `let` or `mut` keyword. Rule 2 of the Typhon language: every local
binding states up-front whether it can be rebound.

## Example

```ty
def main() -> None:
    count = 0          # error: local binding 'count' is missing `let` or `mut`
    count = count + 1
```

## Why

Python's bare `name = value` doubles as both a fresh binding and a rebind of
an existing name. Forcing a keyword keeps intent visible: `let` binds once,
`mut` binds-and-may-rebind.

Module-scope assignments default to `let` and don't need the keyword. Only
function and method bodies trigger this rule.

## Fix

Prefix the binding with the right keyword for your usage:

```ty
def main() -> None:
    mut count: int = 0
    count = count + 1  # ok
```

See https://typhon.dev/lang/diagnostics/missing_binding_kind
