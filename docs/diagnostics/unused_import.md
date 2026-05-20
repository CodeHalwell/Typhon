# tyc::unused_import

Fires when an `import` brings a name into scope but nothing in the module
references it. Configurable via `[strictness] unused-import = "warn" | "error"`
in `typhon.toml`; defaults to `"error"`.

## Example

```ty
import os
import json

def main() -> None:
    print(json.dumps({"ok": True}))
    # `os` is never used
```

## Why

Unused imports accumulate over the life of a file: they slow down module load
time, make dependency tracking opaque, and obscure which third-party packages
a module genuinely depends on.

## Fix

Remove the unused import; if you keep it for a side effect, rename it with a
leading underscore to mark it intentionally unused:

```ty
import json

def main() -> None:
    print(json.dumps({"ok": True}))
```

See https://typhon.dev/lang/diagnostics/unused_import
