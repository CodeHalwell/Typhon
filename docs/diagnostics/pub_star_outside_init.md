# tyc::pub_star_outside_init

Fires (at `Advice` severity) when a `pub *` statement appears in a
regular `.ty` module instead of the package's `__init__.ty`. The
wildcard re-export only has meaning at a package boundary — anywhere
else it's a no-op with confusing intent.

## Example

```ty
# src/mypkg/handlers.ty
pub *   # ☞ tyc::pub_star_outside_init — meaningful only in __init__.ty

pub def handle_request() -> int: return 0
```

## Why

`pub *` is the "re-export every sibling module's public surface"
operator. Its semantics — *aggregate the direct-sibling modules of the
file's parent directory into this file's public surface* — only line
up with the role `__init__.ty` plays in a package. In a regular module
the operator has no meaningful interpretation: there are no siblings
to aggregate (the file's sister modules belong to its **parent**
`__init__.ty`, not to itself), so the build silently does nothing.

Surfaced as advice so the user can move it (or remove it) without
the build failing — but loudly enough that the next `tyc check` run
flags the dead intent.

## Fix

If the file is intended as a package facade, move it to
`__init__.ty`:

```ty
# src/mypkg/__init__.ty
pub *
```

Otherwise drop the `pub *` line — the module's own `pub` declarations
already control its public surface, and the wildcard adds nothing.

## Related

- `tyc::pub_name_collision` — fires when the aggregated `pub *` block
  in `__init__.ty` would re-export the same name from two siblings.

See https://typhon.dev/lang/diagnostics/pub_star_outside_init
