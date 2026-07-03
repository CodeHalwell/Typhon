# tyc::pub_name_collision

Fires when a `pub *` statement in `__init__.ty` aggregates two sibling
modules that both export the same `pub`-marked name. The synthesised
re-exports would silently shadow one with the other depending on import
order, almost always a refactoring slip rather than the intended
behaviour.

## Example

```ty
# src/mypkg/a.ty
pub def hello() -> str:
    return "from a"
```

```ty
# src/mypkg/b.ty
pub def hello() -> str:
    return "from b"
```

```ty
# src/mypkg/__init__.ty
pub *   # ❌ tyc::pub_name_collision — `hello` is exported by both `a` and `b`
```

## Why

The naive aggregation `pub *` performs is:

```ty
# emitted __init__.py for `__init__.ty: pub *`
from .a import hello, …
from .b import hello, …      # ← rebinds `hello` from a → b
__all__ = ["hello", …]
```

Whatever winds up at `mypkg.hello` is whichever sibling import landed
last — fragile, order-sensitive, and surprising. Catching the
collision at build time forces the user to pick one (or both, under
different names) before the package surface goes out the door.

## Fix

Pick one of the conflicting `pub` declarations and either:

- **Rename** one of them so the public surface is unambiguous
  (`pub def hello_from_a()` / `pub def hello_from_b()`).
- **Drop** the `pub` modifier on the implementation you do not want to
  expose so only one sibling contributes the name.
- **Replace `pub *`** with an explicit `from .module import name` list
  so the user makes the choice explicit at the package boundary.

## Related

- `pub *` aggregation is **direct-siblings only**. Transitive
  aggregation through sub-packages is intentionally out of scope —
  each `__init__.ty` re-exports its own children, never its
  grandchildren. A grandchild-level collision would still fire this
  diagnostic at the child's `__init__.ty`.
- `tyc::pub_star_outside_init` — companion advice when `pub *` lands
  in a regular `.ty` module rather than the package's `__init__.ty`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/pub_name_collision.md
