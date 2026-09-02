# tyc::missing_return

Fires when a function declares a non-`None` return type but at least one
execution path reaches the end of the body without `return` or `raise`.

## Example

```ty
def classify(n: int) -> str:
    if n > 0:
        return "positive"
    if n < 0:
        return "negative"
    # falls off the end when n == 0
```

## Why

A function annotated `-> str` promises a `str` on every path. Allowing
fall-off would have Python's interpreter return `None` implicitly and the
caller would receive a `None` where it expected a `str`.

## Fix

Cover the missing path explicitly, or widen the return type to acknowledge
the `None` case.

```ty
def classify(n: int) -> str:
    if n > 0:
        return "positive"
    if n < 0:
        return "negative"
    return "zero"
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/missing_return.md

## Calls that never return

A path that ends in a call that cannot return counts as an exit, so none of
these is "missing a return": `sys.exit(...)`, `exit()`, `quit()`,
`os._exit(...)`, `os.abort()`, `assert False`, or a call to a project function
declared `-> NoReturn` (or `-> Never`). A `-> NoReturn` body itself is exempt
from this diagnostic — its contract is that it always raises or exits.

```ty
def usage() -> NoReturn:
    print("usage: prog FILE")
    sys.exit(2)

def parse(args: list[str]) -> str:
    if len(args) > 1:
        return args[1]
    usage()          # a definite exit — no missing_return
```
