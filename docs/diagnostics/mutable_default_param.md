# tyc::mutable_default_param

Fires when a function parameter's default value is a mutable literal or
constructor call — `def f(xs: list[int] = [])`, `= {}`, `= set()`,
`= dict()`, or a comprehension.

## Example

```ty
def collect(item: int, xs: list[int] = []) -> list[int]:
    # warning: this default list is created ONCE, at `def` time
    xs.append(item)
    return xs

def main() -> None:
    print(collect(1))   # [1]
    print(collect(2))   # [1, 2]  ← shared state, almost never intended
```

## Why

Python evaluates a parameter default once, when the `def` statement
executes — not per call. Every call that omits the argument receives the
*same* object, so mutations accumulate across calls. This is the
canonical Python footgun.

Typhon already rewrites the identical pattern on **class fields**
(`tags: list[str] = []` becomes a per-instance `default_factory`).
Function parameters cannot get the same silent rewrite without changing
the signature that runtime introspection (`inspect.signature`, FastAPI,
documentation tools) observes, so they get this warning instead.

## Fix

Use a `None` sentinel and create the value inside the body:

```ty
def collect(item: int, xs: list[int]? = None) -> list[int]:
    let actual: list[int] = [] if xs is None else xs
    actual.append(item)
    return actual
```

Or, when the default is genuinely meant to be shared module state, hoist
it to an explicit module-level binding so the sharing is visible at the
declaration site.
