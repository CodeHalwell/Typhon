# tyc::invalid_question_op

Fires when the `?` error-propagation operator is used outside a
`Result`-returning function. `?` only makes sense in a context that can
forward an `Err` upward to a caller that expects one.

## Example

```ty
def main() -> int:
    let x: Result[int, str] = try_parse()
    return x?  # error: enclosing function returns `int`, not `Result[_, _]`
```

## Why

`?` is sugar for "if this is `Err`, return it immediately." That only works
when the enclosing function's return type can hold an `Err` of a compatible
shape. Using `?` in a function that returns a plain type would have to
either swallow the error or fabricate a default value, neither of which is
acceptable.

## Fix

Change the enclosing function to return a `Result`, or handle the value with
an explicit `match` instead:

```ty
def main() -> Result[int, str]:
    let x: Result[int, str] = try_parse()
    return Ok(x?)
```

See https://typhon.dev/lang/diagnostics/invalid_question_op
