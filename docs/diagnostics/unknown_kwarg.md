# tyc::unknown_kwarg

Fires when a call site passes a keyword argument whose name doesn't match
any of the callee's parameters (positional or keyword-only) and the callee
has no `**kwargs`. The help text either suggests the closest parameter name
or lists all accepted parameters.

## Example

```ty
def connect(host: str, port: int) -> None: ...

def main() -> None:
    connect(host="localhost", prot=80)  # error: unknown keyword `prot`
```

## Why

The mistake is usually a typo, not a count error — the user knows the
function takes the right number of arguments but spelled one of the names
wrong. A dedicated diagnostic with a "did you mean…" suggestion turns the
typo into a one-second fix.

## Fix

Correct the spelling to match the parameter name:

```ty
def main() -> None:
    connect(host="localhost", port=80)
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/unknown_kwarg.md
