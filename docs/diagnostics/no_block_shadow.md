# tyc::no_block_shadow

Fires when a second `let`/`mut` binding tries to shadow an outer binding of
the same name in the same function. Python's name scoping is function-level,
so what looks like block-scoped shadowing actually rebinds the outer name.

## Example

```ty
def main() -> None:
    let x: int = 1
    if True:
        let x: int = 2  # error: cannot shadow outer `x`
        print(x)
```

## Why

Python doesn't have block scope. The inner `let x` would actually rebind the
*same* function-scoped name as the outer one, leaving callers and readers
confused about which value any later `x` refers to. Rejecting the shadow
matches the runtime semantics.

## Fix

Use a distinct name for the inner value, or drop the keyword to acknowledge
that the outer (mutable) binding is being rebound:

```ty
def main() -> None:
    let x: int = 1
    if True:
        let inner_x: int = 2
        print(inner_x)
```

See https://typhon.dev/lang/diagnostics/no_block_shadow
