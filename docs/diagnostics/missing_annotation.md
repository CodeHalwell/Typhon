# tyc::missing_annotation

Fires when a function parameter or return type lacks an explicit type
annotation. Rule 1 of the Typhon language: every parameter and every return
type is annotated.

## Example

```ty
def greet(name):  # error: parameter `name` is missing a type annotation
    return f"hi {name}"
```

## Why

Annotations are the contract a function signs with its callers. Without them
the checker has to fall back to `Any`, which silently turns every call into
a runtime gamble.

For a function that genuinely returns nothing, write `-> None`.

## Fix

Annotate every parameter and the return type:

```ty
def greet(name: str) -> str:
    return f"hi {name}"
```

For genuinely variadic functions (typically generic decorators) the
canonical idiom for `*args` / `**kwargs` is `object`:

```ty
def trace[R](f: Callable[..., R], *args: object, **kwargs: object) -> R:
    log(f.__name__, args, kwargs)
    return f(*args, **kwargs)
```

Since v0.9.0 the `*args` / `**kwargs` requirement is enforced as part
of Rule 1 (every parameter is annotated). The diagnostic text in v0.9.0
also drops the double-backtick wrapping the previous renderer produced
(was rendered as `` `parameter `x`` ``).

See https://typhon.dev/lang/diagnostics/missing_annotation
