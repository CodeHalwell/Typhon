# tyc::unsafe_value_leak

Fires when a binding introduced inside an `unsafe:` block is returned from a
function whose annotated return type is concrete (e.g. `-> int`) without
being re-asserted at the boundary. Rule 5 in the Typhon language spec: an
unsafe value carries `Unknown` and must cross the safety boundary via a
deliberate re-typing — either an inner annotation that the compiler can
verify (`let typed: int = …`) or an outer re-bind that goes through the
normal assignability check.

## Example

```ty
def parse(raw: object) -> int:
    unsafe:
        let value = raw.maybe_int()      # value: Unknown
    return value                         # error: unsafe value escapes
```

## Why

Inside `unsafe:` the type-checker stops checking — that's the point of the
block. Without an escape audit, the binding flows out with `Unknown` and
the normal `is_assignable(int, Unknown)` rule says "fine" (because
`Unknown` is permissive in both directions). The diagnostic surfaces the
silent contract violation so the user gets a chance to either narrow the
value inside `unsafe:` or re-type at the boundary.

## How to fix

Two idiomatic options:

```ty
# Option A — annotate the unsafe binding so the inner type is concrete.
def parse(raw: object) -> int:
    unsafe:
        let value: int = raw.maybe_int()
    return value

# Option B — re-bind with an annotation outside the unsafe block.
def parse(raw: object) -> int:
    unsafe:
        let value = raw.maybe_int()
    let checked: int = value
    return checked
```
