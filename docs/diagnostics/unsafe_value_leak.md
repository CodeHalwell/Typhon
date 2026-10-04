# tyc::unsafe_value_leak

Fires when a binding introduced inside an `unsafe:` block — or a value
derived from one: `data["name"]`, `data.count + 1`, `[x for x in data]`,
`data.get("k")`, a lambda that reads it — is returned from a function whose
annotated return type is concrete (e.g. `-> int`), or assigned to a
concretely annotated binding, without being re-asserted at the boundary. Rule 5 in the Typhon language spec: an
unsafe value carries `Unknown` and must cross the safety boundary via a
deliberate re-typing — either an annotation on the binding inside the block
that the compiler can verify (`let value: int = …`) or a checked cast at the
boundary (`value as! int`). The help text spells the cast on the escaping
expression (`data["name"] as! str`); when the target has no runtime check
(a `Callable`, a bare type parameter) it suggests only the annotation.

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

# Option B — check the type at the boundary with `as!`.
def parse(raw: object) -> int:
    unsafe:
        let value = raw.maybe_int()
    return value as! int
```

An annotated re-bind outside the block (`let checked: int = value`) is not a
re-assertion: it is itself a concrete boundary, and is reported the same
way.
