# tyc::reserved_module_name

Fires when a project defines its own module or package named `typhon_runtime`
directly under the source root — `src/typhon_runtime.ty`, or a
`src/typhon_runtime/` package.

`typhon_runtime` is reserved for the runtime package `tyc build` generates next
to the emitted Python whenever a program uses `Result` / `Ok` / `Err`, `go`,
`lazy`, `freeze`, `try_result`, or imports `typhon_runtime` itself.

## Example

```text
src/
├── main.ty
└── typhon_runtime/
    └── __init__.ty      # def helper() -> int: …
```

```typhon
# src/main.ty
from typhon_runtime import helper

def f() -> Result[int, str]:
    return Ok(helper() + 1)
```

```text
Error: tyc::reserved_module_name

  × `typhon_runtime` is reserved: this program uses the runtime `tyc build`
  │ generates, which replaces 'src/typhon_runtime', so these imports would
  │ fail when the program starts:
  │   src/main.ty:1: `from typhon_runtime import helper` — `helper` is not
  │   provided by the generated runtime
  help: rename the module (and these imports) — for example to `runtime_helpers`
```

## Why

The generated `build/typhon_runtime/` package is written over the project's
own emitted `typhon_runtime/__init__.py` (and any submodule sharing a name with
a runtime file such as `result.py`), and a package shadows a same-named
`typhon_runtime.py` module. Before this diagnostic the project checked and
built cleanly, then failed at start-up with
`ImportError: cannot import name 'helper' from 'typhon_runtime'`.

## Severity

- **`tyc build` — error** when the build writes the generated runtime *and*
  a project file imports a name (or submodule) from `typhon_runtime` that the
  generated runtime does not provide. That program could not start; the build
  stops before writing the runtime.
- **`tyc build` — warning** in every other case: the project defines the name
  but nothing that is replaced is imported, so the program still runs today.
- **`tyc check` — warning** whenever the project defines the name. Whether the
  build replaces it depends on the desugared program, which `tyc check` does
  not produce.

## Fix

Rename the module and update its imports:

```text
src/typhon_runtime/  →  src/runtime_helpers/
from typhon_runtime import helper  →  from runtime_helpers import helper
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/reserved_module_name.md
