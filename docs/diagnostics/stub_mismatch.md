# tyc::stub_mismatch

Fires when `tyc check --stubs` finds a mismatch between a `.dty` stub and its
implementation module. Stubs are a separate, source-of-truth declaration of
the public API; they must agree with the implementation.

## Example

```text
# helper.dty
def add(a: int, b: int) -> int

# helper.ty
def add(a: int, b: float) -> int:   # error: parameter types differ
    return a + int(b)
```

## Why

Stubs let consumers depend on a module's public surface without loading the
implementation. Drift between the two would silently break downstream
type-checking — the stub would advertise one shape while the runtime offers
another.

## Fix

Sync the stub with the implementation, or — if the symbol is intentionally
private — rename it with a leading underscore so it's excluded from the
stub-coverage check.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/stub_mismatch.md
