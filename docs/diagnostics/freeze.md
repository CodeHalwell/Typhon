# freeze — deep-immutable bindings

`freeze let X = expr` is a module-level binding form that deeply
freezes the bound value. The compiler wraps the RHS in a runtime
helper that recursively replaces every mutable container in the
tree with an immutable equivalent — `list → tuple`, `dict →
MappingProxyType`, `set → frozenset`. The binding itself is `let`
(so it cannot be reassigned) and the value cannot be mutated
through any reference.

## Example

```ty
freeze let TAGS: list[str] = ["a", "b", "c"]
freeze let CONFIG: dict[str, int] = {"port": 8080, "workers": 4}
freeze let WHITELIST: set[int] = {1, 2, 3}
freeze let NESTED: list[dict[str, int]] = [{"x": 1}, {"y": 2}]

def main() -> None:
    # All four bindings raise TypeError on any mutation attempt:
    # TAGS[0] = "z"           # TypeError: 'tuple' does not support assignment
    # CONFIG["port"] = 9999   # TypeError: 'mappingproxy' object does not support assignment
    # WHITELIST.add(4)        # AttributeError: 'frozenset' has no attribute 'add'
    print(TAGS, CONFIG, WHITELIST, NESTED)
```

## How it lowers

```python
from typhon_runtime.freeze import deep_freeze as __typhon_freeze__

TAGS: list[str] = __typhon_freeze__(["a", "b", "c"])
CONFIG: dict[str, int] = __typhon_freeze__({"port": 8080, "workers": 4})
```

The `typhon_runtime` package is generated alongside the build output —
no PyPI install required. The freezer walks the value once at
binding time; subsequent reads pay no overhead.

## Why

`let` alone guarantees that the **binding** cannot be rebound
(`TAGS = ["z"]` is rejected at compile time), but the bound value
might still be mutated through aliasing:

```ty
let TAGS: list[str] = ["a", "b", "c"]
TAGS.append("z")              # silently allowed — let only locks the binding
share_with_caller(TAGS)       # caller can mutate without us knowing
```

`freeze let` closes that gap: even an alias handed to an external
caller cannot mutate the value. Mirrors Rust's `&` reference
behaviour and TypeScript's `readonly` modifier without needing the
host language to support it.

## Coverage

`deep_freeze` handles:

- **Primitives** — `int`, `float`, `bool`, `str`, `bytes`, `None`,
  `complex` (already immutable, passed through)
- **Already-immutable containers** — `tuple`, `frozenset`,
  `MappingProxyType`, `range`, `bytes` (descended into for nested
  values where applicable)
- **Mutable containers** — `list → tuple`, `dict →
  MappingProxyType`, `set → frozenset` (recursively)
- **Frozen dataclasses** — passed through unchanged (their
  field-level frozenness already prevents reassignment)

## What `deep_freeze` rejects

`deep_freeze` raises `TypeError` on anything without a clean
immutable equivalent — file handles, sockets, generators,
non-frozen dataclasses, custom classes that don't declare
`frozen=True`. Better to fail at startup than to silently leak a
mutable alias.

## Scope (current)

The v1 form is module-level only:

```ty
freeze let X = expr   # ✓ module level
def f() -> None:
    freeze let Y = expr   # ✗ rejected — in-function freeze is not yet supported
```

In-function freezing and the `freeze class X:` deep-freeze
field-initialiser form are tracked follow-ups.

See https://typhon.dev/lang/diagnostics/freeze
