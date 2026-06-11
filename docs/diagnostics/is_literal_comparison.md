# tyc::is_literal_comparison

Fires when an `is` / `is not` comparison has a string, number, bytes, or
f-string literal on either side — `s is "hello"`, `n is not 5`.

## Example

```ty
def status_is_done(status: str) -> bool:
    # warning: identity comparison against a literal
    return status is "done"
```

## Why

`is` compares object *identity*, not value. Whether two equal literals
are the same object is an interpreter implementation detail — CPython
caches small integers and interns some strings, so `status is "done"`
can be `True` in one run-shape and `False` in another. CPython itself
emits `SyntaxWarning: "is" with a literal` for this code.

`x is None`, `x is True`, and identity checks against sentinel objects
are the legitimate uses of `is` and are not flagged.

## Fix

Use value equality:

```ty
def status_is_done(status: str) -> bool:
    return status == "done"
```
