# tyc::unknown_name

Fires when an identifier is referenced but no enclosing scope (local,
function-level, module-level, or imported) defines it.

## Example

```ty
def main() -> None:
    print(greting)  # error: cannot find 'greting' in scope
```

## Why

Typhon resolves names statically before the program runs, so a typo or
missing import shows up at build time rather than as a `NameError` at the
first call.

## Fix

Declare the missing name, fix the typo, or add the import that brings it
into scope:

```ty
def main() -> None:
    let greeting: str = "hello"
    print(greeting)
```

If you intended to reference `self`, you'll see `tyc::self_outside_impl`
instead — `self` is only available inside `impl Name:` method bodies.

See https://typhon.dev/lang/diagnostics/unknown_name
