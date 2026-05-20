# tyc::immutable_assign

Fires when a binding declared with `let` is re-assigned later in the same
scope. `let` is Typhon's immutable binding keyword (per Rule 2): once a name
is introduced with `let`, it points at one value for the rest of its lifetime.

## Example

```ty
def main() -> None:
    let count: int = 0
    count = count + 1  # error: cannot assign to immutable binding 'count'
```

## Why

Rule 2 of the Typhon language asks every local to spell out *whether it will
be rebound*. `let` says "no". This makes data flow visible at a glance: a
reader scanning the function knows that any `let` name on the left of `=` is
the original value, not a stale one.

## Fix

Change the declaration keyword to `mut`, which permits re-assignment of new
values of the same declared type:

```ty
def main() -> None:
    mut count: int = 0
    count = count + 1  # ok
```

See https://typhon.dev/lang/diagnostics/immutable_assign
