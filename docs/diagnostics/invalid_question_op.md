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

Which `?` counts as the propagation operator — rather than the nullable
type suffix `T?` — is decided by the same classifier the lowering itself
uses, so the check and the rewrite can never disagree. Earlier releases
recognised only a `?` that followed a closing parenthesis, which left the
first example below (a `?` on a bare name) outside the check entirely: it
surfaced as a `tyc::type_mismatch` naming a synthesised `Err[…]` instead.
A `?` on a bare name inside a comprehension had the same gap and produced
a `tyc::unknown_name` against text the user never wrote.

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

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/invalid_question_op.md
