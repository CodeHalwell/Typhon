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

Since v0.8.0 the conformance check compares parameter types
position-by-position (contravariant on params) in addition to arity, so
a `class BadRepo` claiming to implement `interface Repo: def save(self,
item: str) -> bool` with a `def save(self, item: int) -> bool` impl is
rejected at conformance time.

Since v0.9.0 the arity diagnostic message reads "got N non-self
parameter(s), expected M" instead of the ambiguous "arity N; expected
M" — the previous wording was easy to misread when the impl matched
arity but the parameter type was off, or vice versa.

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
