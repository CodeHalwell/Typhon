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

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/unknown_name.md

## Class attributes inside methods

A class body is not an enclosing scope for the functions defined inside it
(Python's scoping rule), so a method that reads a class attribute by bare
name raises `NameError` at runtime and is reported here. Read it through the
instance or the class instead:

```ty
plain class Registry:
    LIMIT: int = 10

impl Registry:
    def check(self, n: int) -> bool:
        return n < LIMIT         # error: cannot find 'LIMIT' in scope
        # write `self.LIMIT` (or `Registry.LIMIT`)
```

The class's PEP 695 type parameters (`class Box[T]:` / `impl[T] Box[T]:`) are
visible from its methods, as they are in Python.
