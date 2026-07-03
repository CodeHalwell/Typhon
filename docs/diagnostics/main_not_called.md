# tyc::main_not_called

Advice-level diagnostic. Fires when a module declares a top-level `def main()`
but never calls it. A common newcomer mistake — the script's `main` function
never runs, so the build appears successful but produces no output.

## Example

```ty
def main() -> None:
    print("hello")
# advice: `main()` is never invoked
```

## Why

In Python, defining a function only binds the name; the body runs when the
function is called. Modules that define `main` but never invoke it pattern
match the "looks like a runnable script but isn't" mistake closely enough
to surface as advice — but only as advice, since library modules sometimes
export a `main` symbol that's imported elsewhere.

## Fix

Add the standard script-entry pattern at the end of the module:

```ty
def main() -> None:
    print("hello")

if __name__ == "__main__":
    main()
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/main_not_called.md
