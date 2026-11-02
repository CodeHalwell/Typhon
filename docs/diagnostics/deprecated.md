# tyc::deprecated

Fires when code calls something marked deprecated with
[PEP 702](https://peps.python.org/pep-0702/)'s `@deprecated("...")`
(`warnings.deprecated`, or `typing_extensions.deprecated`), or reads a
deprecated property. The marker can come from:

- a dependency, seen through venv introspection. That covers functions,
  classes, methods, property getters and `__call__`. It also covers classes
  you only reach through return values, such as `agent.run_sync(...)` →
  `AgentRunResult` → `.usage` → `RunUsage`.
- your own `.ty` code: `@deprecated("...")` on a function, class, method or
  property.

It is a **warning**: the API still works, and CPython only emits a
`DeprecationWarning` when it runs, which is easy to miss. `tyc check` exits 0
when it is the only finding. Inside an `unsafe:` block it is not reported.

## Example

pydantic-ai turned `AgentRunResult.usage` from a method into a property. It
kept `result.usage()` working by making `RunUsage` callable, with that
`__call__` marked deprecated:

```ty
from pydantic_ai import Agent

def main(agent: Agent) -> None:
    let result = agent.run_sync("hello")
    let u = result.usage()   # warning: `AgentRunResult.usage()` is deprecated:
                             # `usage` is a property, not a method: use `.usage`
                             # instead of `.usage()`.
    print(u)
```

The same applies to your own code:

```ty
from warnings import deprecated

@deprecated("use total() instead")
def sum_all(xs: list[int]) -> int:
    return sum(xs)

def main() -> None:
    print(sum_all([1, 2]))   # warning: `sum_all()` is deprecated: use total() instead
```

## Fix

Follow the deprecation message, which usually names the replacement:

```ty
    let u = result.usage
```

## Related

- `tyc::not_callable` also covers calls on a dependency's return value whose
  class has no `__call__` at all, which would raise `TypeError` at runtime.
- Introspection needs the project's `.venv` (or another interpreter with the
  dependency installed). See `docs/configuration.md` on
  `unintrospectable-dependency`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/deprecated.md
