# tyc::lazy_usage

Fires when a `lazy` construct is used in a form Typhon does not accept for the
project's `[python] target`. The supported shapes are:

- `lazy import name = module` — Typhon's spelling, on every target;
- `lazy import module` / `lazy import module as name` —
  [PEP 810](https://peps.python.org/pep-0810/)'s own spelling, on every target;
- `lazy let NAME: T = expr`;
- `lazy from module import name [as alias], …` — **Python 3.15+ targets only**,
  at the top level of a module.

## Examples

```ty
# typhon.toml: [python] target = "3.13"
lazy from heavy import Thing   # error: `lazy from` needs Python 3.15
```

```ty
# typhon.toml: [python] target = "3.15"
lazy from json import *              # error: PEP 810 forbids a lazy star import
lazy from __future__ import annotations   # error: `__future__` imports cannot be lazy

def load() -> None:
    lazy from json import loads      # error: lazy imports are module-level only
```

## Why

`lazy import module` defers loading a *module*, which Typhon can emulate on any
target with a small proxy. `lazy from module import name` binds each *name* to
a deferred object, which needs the lazy proxy CPython 3.15 provides natively —
there is no faithful lowering for 3.13 / 3.14. On 3.15 Typhon emits the
statement as written, so it inherits PEP 810's rules: no `*`, no `__future__`,
and only at module scope (not inside a function, class or `try` block).

## Fix

On 3.13 / 3.14, defer the module and reach members through it:

```ty
lazy import heavy = heavy
let t = heavy.Thing()
```

or move the project to `[python] target = "3.15"`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/lazy_usage.md
