# tyc::stdlib_module_shadow

Fires when a project `.ty` file's name collides with a Python standard library
top-level module (`types`, `ast`, `string`, `io`, `json`, `dataclasses`,
`logging`, `random`, `time`, …).

## Example

A project with `src/types.ty` and `src/main.ty`:

```text
src/
├── main.ty
└── types.ty   # ⚠ shadows the stdlib `types` module
```

```text
warning: module name `types` shadows the Python stdlib module of the same name
help: the emitted `build/types.py` will be on `sys.path` and intercept
      transitive `import types` from other stdlib modules, producing
      surprising `ImportError`s — rename the file to something stdlib-
      disjoint (e.g. `lang_types.ty`)
```

## Why

`tyc build` emits one `build/<stem>.py` per `.ty` source. The default Python
entry point (`python build/main.py`) puts `build/` on `sys.path`, so a
`build/types.py` will be picked up before the standard library's `types`
module — even by transitive imports from other stdlib packages. The result
is a baffling error like

```text
ImportError: cannot import name 'Ok' from 'typhon_runtime' ...
AttributeError: partially initialized module 'dataclasses' from
  '/usr/lib/python3.13/dataclasses.py' has no attribute 'dataclass'
  (most likely due to a circular import)
```

— blamed on `dataclasses`, but the real culprit is the project's `types.py`
shadowing the stdlib's.

## Fix

Rename the file to something stdlib-disjoint. Common rescuings:

| Conflicting name | Suggested rename |
|------------------|------------------|
| `types.ty`       | `lang_types.ty`, `model_types.ty`  |
| `ast.ty`         | `lang_ast.ty`, `parse_tree.ty`     |
| `string.ty`      | `text.ty`, `str_utils.ty`          |
| `io.ty`          | `app_io.ty`, `streams.ty`          |
| `json.ty`        | `json_codec.ty`, `payload.ty`      |
| `dataclasses.ty` | `records.ty`, `dto.ty`             |
| `logging.ty`     | `logs.ty`, `telemetry.ty`          |

Update every `from <old> import …` site to the new name. The warning is
**non-fatal** — the project still type-checks and builds — so you can ignore
it if you're certain the emitted `build/` directory will not be on
`sys.path` at runtime (for example, you ship via an installed wheel).

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/stdlib_module_shadow.md
