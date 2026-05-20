# tyc::result_error_mismatch

Fires when the error type propagated by `?` from a callee doesn't match the
caller's `Result[T, E]` declaration. The `?` operator forwards the callee's
`Err` value as-is, so the two error types must agree (or be convertible at
the boundary).

## Example

```ty
def step() -> Result[int, ParseErr]: ...

def main() -> Result[int, IOErr]:
    let n: int = step()?  # error: `Err[ParseErr]` propagated into `Result[_, IOErr]`
    return Ok(n)
```

## Why

`?` is sugar for "if `Err`, return it." Without matching error types the
returned `Err` would silently change its type at the propagation boundary,
which defeats the purpose of typed errors. The diagnostic asks the user to
make the conversion explicit.

## Fix

Convert the error at the boundary with a `match`, or change one of the
function signatures so the error types agree:

```ty
def main() -> Result[int, IOErr]:
    match step():
        case Ok(n): return Ok(n)
        case Err(e): return Err(IOErr(str(e)))
```

See https://typhon.dev/lang/diagnostics/result_error_mismatch
