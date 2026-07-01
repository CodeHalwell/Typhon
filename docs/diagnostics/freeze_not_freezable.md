# tyc::freeze_not_freezable

Fires when a `freeze let` binding's right-hand side constructs a value
that cannot be deep-frozen at run time — most commonly a non-`frozen`
user class. `freeze let` recursively converts its value to an immutable
form (`list → tuple`, `dict → MappingProxyType`, `set → frozenset`) via
`typhon_runtime.freeze.deep_freeze`, which raises `TypeError` at startup
on anything without a clean immutable equivalent. This diagnostic catches
that failure at check time instead of letting it surface as a runtime
crash on first import.

## Example

```ty
class Mutable:            # not frozen
    value: int

# tyc::freeze_not_freezable — a non-frozen dataclass can't be deep-frozen
freeze let BAD = Mutable(value=1)

class Immutable frozen:
    value: int

# ✅ frozen dataclasses (and tuples/frozensets/primitives) pass through
freeze let OK = Immutable(value=1)
freeze let CONFIG = {"port": 8080, "hosts": ["a", "b"]}
```

## Fix

Declare the class `frozen` (`class Point frozen:`), or use an already
deep-freezable value (primitives, tuples, frozensets, or nested
lists/dicts/sets of those). File handles, sockets, generators, and
non-frozen dataclasses have no immutable equivalent and cannot be frozen.

## See also

- `tyc::frozen_assign` — writing a field on a `frozen` class.
- `tyc::immutable_assign` — reassigning a `freeze let` / `let` binding.
