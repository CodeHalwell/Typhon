# tyc::implicit_any

Fires when a bare collection annotation (`list`, `dict`, `tuple`, `set`,
`frozenset`) appears without its element-type parameters. Per Typhon Rule 1
and `[strictness] no-implicit-any = true` (the default), every container
annotation must spell out its element types.

## Example

```ty
def keys(d: dict) -> list:  # error: bare `dict` / `list` have implicit Any
    return list(d.keys())
```

## Why

`list` without a parameter implicitly means `list[Any]`, which silently
disables every meaningful type check that touches the value. The strict
default forces the user to record what's inside the container, which makes
both the call sites and the body type-checkable.

## Fix

Add element-type parameters that match what the function actually consumes
and produces:

```ty
def keys(d: dict[str, int]) -> list[str]:
    return list(d.keys())
```

See https://typhon.dev/lang/diagnostics/implicit_any
