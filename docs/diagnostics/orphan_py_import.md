# tyc::orphan_py_import

Warns when a `from .NAME import …` references a sibling `.py` file that lives
outside the project's `src/` tree. `tyc build` only copies files under `src/`
into the output directory, so the emitted Python would crash at import time
with `ModuleNotFoundError`.

## Example

```ty
# src/main.ty
from .helper import do_thing  # warning: helper.py lives outside src/
```

## Why

The build pipeline is conservative about what it copies into the output: only
files inside `src/` are eligible. A relative import that resolves to a `.py`
file outside that root would still typecheck locally (the file *exists* on
disk) but disappear at runtime once the build is shipped.

## Fix

Move the helper under `src/`, or rewrite the import as a project-relative
absolute import that names a module the build does package:

```ty
# src/main.ty
from myproject.helper import do_thing
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/orphan_py_import.md
