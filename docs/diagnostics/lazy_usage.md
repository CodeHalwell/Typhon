# tyc::lazy_usage

Fires when a `lazy` construct is used in an unsupported form. The supported
shapes are `lazy import name = module`, `lazy let NAME: T = expr` and, on a
3.15+ `[python] target`, a module-level `lazy from module import names`.

## Example

```ty
lazy from heavy import *              # error: star imports cannot be lazy
lazy from __future__ import annotations  # error: nor can __future__ imports

def load() -> None:
    lazy from heavy import Thing      # error: only at module level
```

## Why

`lazy` carries specific semantics: defer evaluation until first access. The
supported forms each have a well-defined lowering (a sentinel-cached
evaluation for `lazy let`, a lazy module proxy or PEP 810's native statement
for `lazy import`, PEP 810's native `lazy from` on 3.15). The shapes above are
`SyntaxError`s in CPython 3.15 too: a star import has no names to defer, a
`__future__` import changes how the module compiles, and PEP 810 only defers
module-level imports.

A `lazy from` on a 3.13 or 3.14 target is reported as
[`tyc::requires_newer_python`](requires_newer_python.md) instead.

## Fix

Name the imports you need, move the import to module level, or use the form
every target has:

```ty
lazy import heavy = heavy
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/lazy_usage.md
