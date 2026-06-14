# Lesson 5 — Error handling with `Result`

*Zero to Hero · Lesson 5 of 10*

This lesson covers Rule 7: expected failures are **values**, not exceptions.

## `Result[T, E]`, `Ok`, and `Err`

`Result[T, E]` is a sealed type with two cases, `Ok(value)` and `Err(error)`. A function that can fail says so in its return type. From `examples/07-error-handling/error_handling.ty`:

```python
class ParseError:
    field: str
    reason: str


def parse_port(raw: str) -> Result[int, ParseError]:
    if not raw.isdigit():
        return Err(ParseError(field="port", reason=f"not a number: {raw}"))
    let n: int = int(raw)
    if n < 1 or n > 65535:
        return Err(ParseError(field="port", reason=f"out of range: {n}"))
    return Ok(n)
```

The error type can be anything — a `str`, a class, a sealed union. `Ok` and `Err` come from the generated `typhon_runtime` (no import needed in your source).

## The `?` operator

`?` unwraps an `Ok` and short-circuits an `Err` out of the current function. It is **not** `try/except` — it desugars to a plain `isinstance` check plus an early `return`, so stack traces stay clean:

```python
def parse_addr_short(host_raw: str, port_raw: str) -> Result[tuple[str, int], ParseError]:
    let host: str = parse_host(host_raw)?     # unwrap Ok, or return the Err
    let port: int = parse_port(port_raw)?
    return Ok((host, port))
```

The checker enforces two things:

- `?` only appears inside a function that returns a compatible `Result` (else `tyc::invalid_question_op`).
- The error types must line up (else `tyc::result_error_mismatch`) — convert at the boundary with `match` or `.map_err`.

## `with`-chains

For several dependent steps that share error handling:

```python
def parse_addr(host_raw: str, port_raw: str) -> Result[tuple[str, int], ParseError]:
    with host = parse_host(host_raw)?,
         port = parse_port(port_raw)?:
        return Ok((host, port))
    else err:
        print(f"failed parsing {err.field}: {err.reason}")
        return Err(err)
```

The `else err:` block is optional; without it, the first `Err` short-circuits through the enclosing function.

## Consuming a `Result`

You `match` on it — and because `Ok`/`Err` are the only two cases, no `_` fallthrough is needed:

```python
match parse_addr("localhost", "8080"):
    case Ok((host, port)):
        print(f"bound to {host}:{port}")
    case Err(e):
        print(f"failed: {e.reason}")
```

Combinators chain transformations without unwrapping: `.map(f)` transforms an `Ok` value, `.map_err(g)` transforms an `Err`, `.and_then(h)` chains another `Result`-returning step, `.or_else(k)` recovers.

## Bridging libraries that throw

Wrap a throwing boundary once — either a small `try` shim or the one-expression `try_result`:

```python
import json

def parse_json(text: str) -> Result[dict[str, object], str]:
    return try_result(lambda: json.loads(text), lambda e: f"invalid JSON: {e}")
```

`try_result(thunk, on_err)` runs `thunk()`, returning `Ok(result)` or, on any exception, `Err(on_err(exc))`. After the boundary, everything downstream uses `?` and never sees an exception.

## Common mistakes

**`?` in a non-`Result` function:**

```python
def total(raw: str) -> int:
    let n: int = parse_port(raw)?      # ❌ tyc::invalid_question_op
```

Fix: change the return type to `Result[int, …]`, or `match` explicitly.

**Mismatched error types** crossing a `?`:

```python
# parse returns Result[int, ParseError]; function returns Result[int, str]
let n: int = parse_port(raw)?          # ❌ tyc::result_error_mismatch
```

Fix: `parse_port(raw).map_err(lambda e: e.reason)?`.

## Try it

1. Write `def safe_div(a: int, b: int) -> Result[int, str]:` returning `Err("division by zero")` when `b == 0`, else `Ok(a // b)`.
2. Write `def average(xs: list[int]) -> Result[float, str]:` that uses `safe_div`-style logic via `?` and returns `Err` for an empty list.
3. `match` the result in `main` and print each branch.

## What you learned

- `Result[T, E]` with `Ok`/`Err` makes failure an explicit value.
- `?` propagates `Err` cleanly; `with`-chains sequence several fallible steps.
- `try_result` (or a `try` shim) bridges libraries that raise.

**Next:** [Lesson 6 — Sealed unions and `match`](lesson-06-sealed-unions-and-match.md).
