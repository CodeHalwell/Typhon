# tyc::invalid_question_op

Fires in two situations:

1. **The `?` operator appears outside a `Result`-returning function.**
   `?` only makes sense in a context that can forward an `Err`
   upward to a caller that expects one.
2. **The `?` operator appears inside a comprehension.** Comprehensions
   lower to nested loops in Python, so the surrounding function frame
   that `?` would short-circuit out of is not the comprehension's
   frame. `?` inside a `for ... in ...` clause or a generator
   expression is rejected with an explicit hint pointing users at the
   `?`-then-comprehend form.

Since v0.9.0 the help text mentions both causes explicitly so the
diagnostic is actionable in either situation.

## Example — Result-return cause

```ty
def main() -> int:
    let x: Result[int, str] = try_parse()
    return x?  # error: enclosing function returns `int`, not `Result[_, _]`
```

## Example — comprehension carve-out

```ty
def collect(xs: list[str]) -> Result[list[int], str]:
    return Ok([parse(x)? for x in xs])  # error: `?` inside a comprehension
```

## Why

`?` is sugar for "if this is `Err`, return it immediately." That only works
when the enclosing function's return type can hold an `Err` of a compatible
shape, and only when the surrounding scope is a function (not a
comprehension frame).

## Fix

Change the enclosing function to return a `Result`, or pre-extract the
values with an explicit loop:

```ty
def main() -> Result[int, str]:
    let x: Result[int, str] = try_parse()
    return Ok(x?)

def collect(xs: list[str]) -> Result[list[int], str]:
    mut out: list[int] = []
    for x in xs:
        out.append(parse(x)?)
    return Ok(out)
```

See https://typhon.dev/lang/diagnostics/invalid_question_op
