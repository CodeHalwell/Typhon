# tyc::frozen_assign

Fires when a field of a `frozen` class is assigned outside the constructor.
`frozen` makes a class immutable per instance: every field is set once at
construction time and then cannot change.

## Example

```ty
frozen class Identity:
    name: str

def main() -> None:
    let id: Identity = Identity(name="Alice")
    id.name = "Bob"  # error: cannot assign to field 'name' on frozen class
```

## Why

Frozen classes guarantee structural identity: two `Identity` values with
matching fields compare equal forever, hash stably, and never surprise a
reader by mutating across calls. Permitting in-place assignment would
silently break those guarantees.

## Fix

Construct a new value with the desired field overrides instead of mutating:

```ty
def main() -> None:
    let id: Identity = Identity(name="Alice")
    let id2: Identity = Identity(name="Bob")  # new value, not a mutation
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/frozen_assign.md
