# tyc::incompatible_override

Fires when a subclass method overrides a base-class method with an
incompatible signature: a different parameter count, a parameter type
*narrower* than the base's, or a return type not assignable to the
base's.

## Example

```ty
class Base:
    tag: str

impl Base:
    def handle(self, x: object) -> str:
        return "base"

class Sub(Base):
    extra: str

impl Sub:
    # warning: parameter 1 narrows the base's `object` to `int`
    def handle(self, x: int) -> str:
        return "sub"
```

## Why

Code holding a `Base` may call `handle` with anything `object` allows —
and dispatch to `Sub.handle` at runtime, which only accepts `int`. This
is the Liskov substitution principle: an override may **widen** its
parameters and **narrow** its return type, never the reverse.

The check is conservative: underscore-prefixed methods, property /
staticmethod / classmethod binding differences, variadic signatures
(`*args` / `**kwargs`), and parameters the checker can't type
confidently are all skipped.

## Fix

Match (or widen) the base signature:

```ty
impl Sub:
    def handle(self, x: object) -> str:
        guard n = x if isinstance(x, int) else None else:
            return "sub: not a number"
        return f"sub: {n}"
```

Or, if the subclass genuinely needs a different contract, give the
method a new name instead of overriding.
