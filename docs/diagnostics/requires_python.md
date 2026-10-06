# tyc::requires_python

Fires when a program uses a builtin that only exists from a later CPython
release than the project's `[python] target`. Today that is `frozendict`
(Python 3.15, [PEP 814](https://peps.python.org/pep-0814/)) on a `3.13` or
`3.14` target. The emitted Python would raise `NameError` the moment that line
ran on the target interpreter, so the program is rejected at check time
instead.

Annotations count too: `def f(m: frozendict[str, int])` names a type the
target does not have.

## Example

```ty
# typhon.toml: [python] target = "3.13"
let defaults = frozendict(retries=3, timeout=30)   # error: frozendict is new in Python 3.15
```

## Why

Typhon checks the program against the interpreter it will actually run on.
`frozendict` cannot be emulated faithfully on 3.13 / 3.14 — it is a builtin
type, hashable, and recognised by `json`, `copy` and `pickle` — so there is no
lowering that keeps the semantics.

A module that binds the name itself is left alone: with
`from frozendict import frozendict` (the PyPI backport) the name is the
import, not the builtin.

## Fix

Target Python 3.15:

```toml
[python]
target = "3.15"
```

or, on an older target, use a read-only view or a frozen value:

```ty
from types import MappingProxyType
let defaults = MappingProxyType({"retries": 3, "timeout": 30})
freeze let DEFAULTS = {"retries": 3, "timeout": 30}   # deep-frozen at startup
```

See also `tyc::removed_in_python` (the other direction: an API the target no
longer has) and `docs/configuration.md` (`[python] target`).
