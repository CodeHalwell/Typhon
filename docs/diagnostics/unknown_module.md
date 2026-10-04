# tyc::unknown_module

Fires when an `import` (or `from … import …`) names a module that isn't in
the Python stdlib, isn't part of the project, isn't bundled in
`typhon_runtime`, and isn't listed under `[dependencies]` in `typhon.toml`.

It is a **warning** — the module may still be installed in the environment
the program runs in — so `tyc check` exits 0 when it is the only finding.

## Example

```ty
import flask  # warning if flask is not declared in typhon.toml
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

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/unknown_module.md
