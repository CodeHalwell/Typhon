# tyc::interface_isinstance

Fires when `isinstance(x, SomeInterface)` is used without an opt-in.
Structural interfaces describe a shape, not a runtime class — Python's
`isinstance` would only verify attribute presence, which is a weaker check
than the static interface conformance guarantees.

## Example

```ty
interface Writer:
    def write(self, data: str) -> int

def main(x: object) -> None:
    if isinstance(x, Writer):  # error: rejected unless opted in
        ...
```

## Why

Structural typing checks every member's *signature* statically; a runtime
`isinstance` only checks that an attribute with the right *name* exists. The
two checks are easy to confuse and the runtime form silently weakens the
guarantee, so the default is to reject it and prefer static narrowing.

## Fix

Decorate the interface with `@runtime_checkable` if you genuinely need an
attribute-presence check, or rely on static structural typing instead:

```ty
from typing import runtime_checkable

@runtime_checkable
interface Writer:
    def write(self, data: str) -> int
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/interface_isinstance.md
