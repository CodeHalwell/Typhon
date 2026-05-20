# tyc::unknown_module

Fires when an `import` (or `from … import …`) names a module that isn't in
the Python stdlib, isn't part of the project, isn't bundled in
`typhon_runtime`, and isn't listed under `[dependencies]` in `typhon.toml`.

## Example

```ty
import flask  # error if flask is not declared in typhon.toml
```

## Why

The import would later fail at runtime with a `ModuleNotFoundError`, often
deep inside an unrelated build step. Catching the typo (or missing dependency
declaration) at check time keeps the error close to its cause.

## Fix

Either correct the spelling, add the dependency to `typhon.toml` and run
`tyc sync`, or create the missing sibling `.ty` file.

```toml
# typhon.toml
[dependencies]
flask = "^3.0"
```

See https://typhon.dev/lang/diagnostics/unknown_module
