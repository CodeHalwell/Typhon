# tyc::requires_python

Fires when a program uses something that only exists from a later CPython
release than the project's `[python] target`. The emitted Python would raise
`NameError`, `ImportError` or `AttributeError` the moment that line ran on the
target interpreter, so the program is rejected at check time instead.

Covered today:

| New in | What |
|---|---|
| 3.15 | the builtins `frozendict` ([PEP 814](https://peps.python.org/pep-0814/)) and `sentinel` ([PEP 661](https://peps.python.org/pep-0661/)) |
| 3.15 | the `profiling` package (PEP 799) and `math.integer` (PEP 791) |
| 3.15 | `typing.TypeForm` (PEP 747), `typing.disjoint_base` (PEP 800), `math.fmax` / `fmin` / `isnormal` / `issubnormal` / `signbit`, `re.prefixmatch`, `sys.set_lazy_imports` / `get_lazy_imports`, `threading.synchronized_iterator` / `serialize_iterator` / `concurrent_tee`, `types.LazyImportType` / `FrameLocalsProxyType` |
| 3.14 | the `annotationlib` (PEP 749), `compression` (PEP 784), `string.templatelib` (PEP 750) and `concurrent.interpreters` (PEP 734) modules |

A builtin counts wherever it is named, annotations included
(`def f(m: frozendict[str, int])`). A stdlib module or name counts in an
`import`, a `from … import`, or a `module.name` read through a plain
`import module`.

## Example

```ty
# typhon.toml: [python] target = "3.13"
let defaults = frozendict(retries=3, timeout=30)   # error: frozendict is new in Python 3.15
from typing import TypeForm                          # error: typing.TypeForm is new in Python 3.15
```

## Why

Typhon checks the program against the interpreter it will actually run on.
None of these can be emulated faithfully on an older target — `frozendict` is
a builtin type recognised by `json`, `copy` and `pickle`; a `sentinel` is a
distinct singleton type — so there is no lowering that keeps the semantics.

Names that are not the stdlib's are left alone: a module that binds
`frozendict` itself (`from frozendict import frozendict`, the PyPI backport),
a project module called `profiling.ty`, and a declared dependency of the same
name as a new stdlib module are what the code refers to.

## Fix

Target the release that has it:

```toml
[python]
target = "3.15"
```

or, on an older target, use what it has:

```ty
from types import MappingProxyType
let defaults = MappingProxyType({"retries": 3, "timeout": 30})
freeze let DEFAULTS = {"retries": 3, "timeout": 30}   # deep-frozen at startup
from typing_extensions import TypeForm                # the backport
```

See also `tyc::removed_in_python` (the other direction: an API the target no
longer has) and `docs/configuration.md` (`[python] target`).
