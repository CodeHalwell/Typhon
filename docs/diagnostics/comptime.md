# tyc::comptime

Fires when a `comptime` binding's right-hand side cannot be evaluated at
build time — either the expression uses an unsupported operation or an
input (such as `env(...)`) is unavailable.

## Example

```ty
comptime let PORT: int = int(env("PORT"))  # error if PORT env var is unset
```

## Why

`comptime` is for values that must be known *before* the program runs, so
the build can inline them as literals. The supported subset of Typhon is
deliberately small (literals, arithmetic, comparisons, boolean ops, a few
string methods, `env(...)`, calls to other `comptime def` functions) —
anything outside that subset has to wait until runtime.

## Fix

Supply the missing env var, or move the computation to runtime if it depends
on state that isn't known at build time:

```ty
import os
let PORT: int = int(os.environ.get("PORT", "8080"))
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/comptime.md
