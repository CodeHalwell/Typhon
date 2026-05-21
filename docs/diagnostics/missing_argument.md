# tyc::missing_argument

Fires when a function, method, or class constructor is called without
filling one or more required parameters. Surfaces the *names* of the
missing arguments so the fix is immediate — distinct from
[`tyc::arg_count`](./arg_count.md), which only reports counts and is
emitted for shape mismatches that don't reduce to "you forgot
parameter X" (e.g. too many positional args, conflicting
positional + kwarg for the same parameter).

## Example

The class case is the most common — every required field of a
generated `__init__` must be supplied, by position or by keyword:

```ty
from agent_framework import Agent

def main() -> None:
    let a: Agent = Agent(
        name="WebSearchAgent",
        tools=[],
        description="…",
        instructions="…",
    )
    # error: missing required argument to `Agent`: `client`
```

The fix names itself:

```ty
def main() -> None:
    let a: Agent = Agent(
        client=chat_client,    # supply the missing required arg
        name="WebSearchAgent",
        tools=[],
        description="…",
        instructions="…",
    )
```

## Free functions

```ty
def connect(host: str, port: int, timeout: float = 5.0) -> None: ...

def main() -> None:
    connect(timeout=2.0)
    # error: missing required arguments to `connect`: `host`, `port`
```

## Why a separate code from `tyc::arg_count`

The historical message ("expected 2, got 1") buries the actionable
detail when the call already provided arguments for *other*
parameters — the caller sees "got 1" and reads it as too many, not
too few. `missing_argument` reports exactly which names weren't
filled, so the diff to fix is mechanical.

The checker falls back to `tyc::arg_count` whenever it can't identify
specific missing names (e.g. too many positional arguments, or a
keyword conflicting with a positional already supplied).

## Cross-module imports

Third-party classes participate on equal footing. Venv-introspected
shapes (recovered from installed packages without a `.dty` stub) are
matched against the call site the same way as in-project classes —
so `from agent_framework import Agent; Agent(name=...)` is flagged
the same as `from local_lib import Agent`.

See https://typhon.dev/lang/diagnostics/missing_argument
