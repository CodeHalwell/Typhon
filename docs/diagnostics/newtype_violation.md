# tyc::newtype_violation

Fires when a bare value of the base type is passed into a `newtype`
constructor without satisfying the base contract, or when a bare base
value is supplied where the newtype is expected without going through
the explicit constructor.

## Example

```ty
newtype UserId = int
newtype PostId = int

def greet(uid: UserId) -> str:
    return f"hi {uid}"

def main() -> None:
    # error: `UserId(str)` — newtype expects an argument of type `int`
    let bad: UserId = UserId("seven")

    let post: PostId = PostId(42)
    # error: type mismatch — expected `UserId`, found `PostId`
    greet(post)
```

## Why

`newtype` is the nominal-alias escape hatch for stopping interchangeable
primitives from being mixed up — order ids vs. cart ids, USD vs. EUR,
internal user ids vs. external user ids. The compiler treats `UserId`
and `int` as **asymmetric**:

- `UserId` flows freely into an `int`-typed slot (the runtime values
  are identical, so the escape upward is always safe).
- A bare `int` does **not** satisfy a `UserId`-typed slot. The caller
  must opt in by writing `UserId(x)`, which type-checks `x` against
  `int` and yields a `UserId`-typed value.

This makes the boundary between a domain id and a raw primitive
visible at every call site, in exchange for one extra constructor call
at the moment a domain value enters the system.

## Fix

If the value really is a `UserId`, wrap it explicitly:

```ty
let raw: int = 7
let uid: UserId = UserId(raw)
```

If the call site genuinely wants the base type, adjust the parameter
annotation to `int` instead of `UserId`. If you want bidirectional
substitutability, use `type` (a transparent alias) instead of `newtype`:

```ty
type Width = int   # int and Width are interchangeable in both directions
```

## How it lowers

`newtype Name = Base` lowers to `Name = NewType("Name", Base)` (Python's
`typing.NewType`) at compile time, and `from typing import NewType` is
injected into the emitted module as needed. The wrapper call is a no-op
at runtime — `NewType` returns its argument unchanged — so the nominal
distinction has zero runtime cost.

See https://typhon.dev/lang/diagnostics/newtype_violation
