# tyc::interface_not_conforming

Fires when a value of some concrete type is used where an `interface` is
required and the concrete type is missing one or more of the interface's
members (or supplies a member with an incompatible signature).

## Example

```ty
interface Writer:
    def write(self, data: str) -> int

class Sink:
    pass  # no `write` method

def emit(w: Writer, data: str) -> int:
    return w.write(data)

def main() -> None:
    emit(Sink(), "hi")  # error: `Sink` does not conform to `Writer`
```

## Why

Interfaces in Typhon are structural: a value conforms iff it exposes every
required member with a matching signature. The check is performed at the
call site rather than at class-definition time, so any usage where the
required shape is missing fails immediately and points at the offending
argument.

## Fix

Add the missing member(s) to the concrete type with the right parameter and
return types, or supply a value that already conforms:

```ty
class Sink:
    pass

impl Sink:
    def write(self, data: str) -> int:
        return len(data)
```

See https://typhon.dev/lang/diagnostics/interface_not_conforming
