# tyc::self_outside_impl

Fires when `self` is referenced outside an `impl ClassName:` method body.
`self` is only meaningful inside a method that's bound to a class instance.

## Example

```ty
def length(self) -> float:  # error: `self` is not available here
    return 0.0
```

## Why

Per Typhon Rule 4, methods take an explicit `self` and live inside `impl`
blocks; free functions have no instance to refer to. A bare `self` in a
free function would silently bind to an out-of-scope name — flagging it as
a distinct diagnostic (rather than the generic `tyc::unknown_name`) lets
the help text point users at the right shape.

## Fix

Move the function into an `impl` block, or replace `self` with an explicit
parameter:

```ty
class Point:
    x: int
    y: int

impl Point:
    def length(self) -> float:
        return (self.x * self.x + self.y * self.y) ** 0.5
```

See https://typhon.dev/lang/diagnostics/self_outside_impl
