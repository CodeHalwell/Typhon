# tyc::removed_in_python

Fires when a program imports or uses a stdlib API that the project's
`[python] target` no longer ships, or calls a stdlib form that release stopped
accepting. The emitted Python would raise `ImportError`, `AttributeError` or
`TypeError` on the target interpreter.

Covered today (each only from the release that removed it):

| Removed in | API | Use instead |
|---|---|---|
| 3.14 | `ast.Num`, `ast.Str`, `ast.Bytes`, `ast.NameConstant`, `ast.Ellipsis` | `ast.Constant` |
| 3.14 | `importlib.abc.ResourceReader` / `Traversable` / `TraversableResources` | `importlib.resources.abc` |
| 3.14 | `pkgutil.find_loader`, `pkgutil.get_loader` | `importlib.util.find_spec` |
| 3.14 | `pty.master_open`, `pty.slave_open` | `pty.openpty` |
| 3.14 | `sqlite3.version`, `sqlite3.version_info` | `sqlite3.sqlite_version` / `sqlite_version_info` |
| 3.14 | `urllib.request.URLopener`, `FancyURLopener` | `urllib.request.urlopen` |
| 3.15 | the `sre_compile`, `sre_constants`, `sre_parse` modules | `re` |
| 3.15 | `glob.glob0`, `glob.glob1` | `glob.glob(pattern, root_dir=…)` |
| 3.15 | `typing.no_type_check_decorator` | drop it |
| 3.15 | `NamedTuple("P", x=int)` — keyword fields | `NamedTuple("P", [("x", int)])` or a class |
| 3.15 | `TypedDict("T")` / `TypedDict("T", None)` — no fields | `TypedDict("T", {})` or a class |

## Example

```ty
# typhon.toml: [python] target = "3.15"
from typing import NamedTuple
let Point = NamedTuple("Point", x=int, y=int)   # error: keyword fields were removed in Python 3.15
```

## Why

3.13 and 3.14 only warned (`DeprecationWarning`) about most of these; 3.15
removes them. A project that moves its target forward finds every use at check
time rather than one crash at a time in production.

The check is syntactic: an import, a `module.name` read through a plain
`import module`, or a call to the `typing` callable by name. Nothing is
reported on a target that still has the API.

## Fix

Follow the replacement in the table, or keep the project on the older
`[python] target` until the code is migrated.

See also `tyc::requires_python` (an API the target does not have *yet*).
