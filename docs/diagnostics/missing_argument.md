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

## Third-party introspection: scope and limits

For an imported third-party package with no `.dty` stub, `tyc check` /
`tyc build` shell to the project's `.venv/bin/python` (or a `python3` on
PATH) and run `inspect.signature` over each public class / function. This
recovers real signatures **without** a hand-written stub, but it has
boundaries worth knowing:

| Mistake | Caught at compile time? |
|---|---|
| Missing required argument (function or constructor) | ✅ |
| Unknown keyword when the target has no `**kwargs` | ✅ |
| **Wrong *type* of a free-function argument** (e.g. `int` where `str` is annotated) | ✅ since the annotation-capture pass — *for fully type-annotated, pure-Python libraries* |
| **Wrong *type* of a constructor argument** (e.g. `port="oops"` where `port: int`) | ✅ since the annotation-capture pass (same caveat) |
| Too many positional args to a **constructor with ≥1 field** | ✅ via the constructor arity check |
| Too many positional args to a **zero-field constructor** (`Session(1)`) | ✅ for venv-introspected and normal fully-known classes; `plain class` / `class!` are exempt (their fields may not reflect a hand-written `__init__`) |
| Unknown keyword when the target has `**kwargs` | ❌ — correct: `**kwargs` legitimately absorbs it |
| **C-extension / built-in callables** (numpy, pandas, pydantic-core, torch, …) | ❌ — `inspect.signature` raises, so the symbol is skipped (a missing check is safer than a wrong one) |
| Anything when the package is **not installed in a reachable venv** | ❌ — introspection silently degrades to "no info" |

Two consequences follow from how this works:

- **Argument-type checking depends on inline annotations.** The capture
  pass reads each parameter's `annotation` and maps the scalar builtins
  (`int` / `str` / `bool` / `float` / `bytes` / `None`), the nullable forms
  `Optional[X]` / `X | None`, and the parametric containers `list[X]` /
  `set[X]` / `frozenset[X]` / `dict[K, V]` (recursively) to Typhon types;
  everything else (multi-member unions, `tuple`, `Callable`, foreign
  classes) conservatively degrades to a permissive `Unknown`, so a library
  you can't fully model never produces a false positive. Libraries whose
  types live
  only in **typeshed stubs** (rather than inline annotations) — `requests`
  is the classic example — therefore get arity checking but not argument-
  *type* checking from this path.
- **Complete "every library" type checking needs typeshed.** Runtime
  introspection alone can't see C-extension signatures or typeshed-only
  annotations. The roadmap item for that is the typeshed-backed `ty`
  second-stage checker (see `docs/ty-integration.md`).

See https://typhon.dev/lang/diagnostics/missing_argument
