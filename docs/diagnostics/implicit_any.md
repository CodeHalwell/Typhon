# tyc::implicit_any

Fires when a bare collection annotation (`list`, `dict`, `tuple`, `set`,
`frozenset`) appears without its element-type parameters. Per Typhon Rule 1
and `[strictness] no-implicit-any = true` (the default), every container
annotation should spell out its element types.

## Where it fires

The check runs on **annotated-assignment** positions — locals, module-level
bindings, and class-body field declarations:

```ty
def main() -> None:
    let xs: list = [1, 2, 3]        # error: bare `list` has an implicit Any element

CACHE: dict = {}                    # error: bare `dict`

class Bag:
    items: list                     # error: bare `list`
```

## Where it does *not* fire (known gap)

Function **parameter** and **return** annotations are not covered today:

```ty
def keys(d: dict) -> list:          # accepted — no diagnostic, though it should be
    return list(d.keys())
```

Both are still `Any`-shaped and still defeat downstream checking, so treat the
signature form as just as wrong even though the compiler is currently silent
about it. Closing the gap is a deliberate narrowing of the accepted surface
(it would reject programs that run correctly today), so it is tracked rather
than applied silently.

## Why

`list` without a parameter implicitly means `list[Any]`, which silently
disables every meaningful type check that touches the value. The strict
default forces the author to record what's inside the container, which makes
both the call sites and the body type-checkable.

## Fix

Add element-type parameters that match what the code actually consumes and
produces:

```ty
def keys(d: dict[str, int]) -> list[str]:
    return list(d.keys())
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/implicit_any.md
