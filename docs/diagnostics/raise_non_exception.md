# tyc::raise_non_exception

Fires when a `raise` operand is provably not an exception — a literal, another
primitive, or an instance of a user class that does not derive from
`BaseException`.

## Example

```ty
class Problem:
    msg: str

def do_work(flag: bool) -> int:
    if flag:
        raise Problem(msg="bad input")   # error: Problem is not an exception
    return 1
```

```ty
def f() -> int:
    raise 42                              # error: cannot raise an int
```

## Why

CPython requires the operand of `raise` to be a `BaseException` subclass or
instance; anything else raises `TypeError: exceptions must derive from
BaseException` at runtime. The program type-checked clean but was guaranteed to
crash, so Typhon rejects it up front (mypy and pyright reject it too).

The check is deliberately conservative: it only fires when the operand's type
is fully known and certainly not an exception. A builtin exception
(`raise ValueError(...)`), a user class that subclasses `Exception`, a class
that inherits from an imported/unknown base, and a re-raised caught exception
all pass.

## Fix

Raise an actual exception, or — for a recoverable error you want a caller to
handle by value — return `Err(...)` instead of raising:

```ty
class ConfigError(Exception):
    msg: str

def do_work(flag: bool) -> int:
    if flag:
        raise ConfigError(msg="bad input")   # ok: derives from Exception
    return 1
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/raise_non_exception.md
