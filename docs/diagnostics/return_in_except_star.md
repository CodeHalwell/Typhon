# tyc::return_in_except_star

Fires when a `return`, `break`, or `continue` appears inside an `except*`
handler body.

## Example

```ty
def handle() -> int:
    try:
        raise ExceptionGroup("g", [ValueError("bad")])
    except* ValueError:
        return 1                       # error: `return` in an `except*` block
    return 0
```

```ty
def scan(items: list[str]) -> None:
    for item in items:
        try:
            check(item)
        except* ValueError:
            continue                   # error: `continue` in an `except*` block
```

## Why

CPython rejects all three keywords inside an `except*` block at **compile**
time:

```
SyntaxError: 'break', 'continue' and 'return' cannot appear in an except* block
```

The reason is PEP 654's execution model: an `except*` handler is not a single
branch. The raised exception group is *split*, and the interpreter may run a
handler body more than once — once per matching subgroup — before re-raising
whatever remained unhandled. A `return` out of the middle of that process has
no defined meaning, so the language forbids it outright.

Without this diagnostic, Typhon accepted the program, `tyc check` reported no
errors, `tyc build` reported success, and the emitted `build/main.py` could not
even be imported. That is the worst shape of defect a compiler can ship: a
green pipeline whose artifact is not valid Python, and which a CI job gating on
`tyc check` would never catch.

### Compatibility

This diagnostic is a **narrowing on already-crashing code**, the same carve-out
the three v1.0.0-alpha.2 diagnostics took. Every program it rejects emitted
Python that CPython refused to compile, so no program that ever *ran*
correctly is affected. Typhon's "additive on correct programs" guarantee is
intact.

## What is and is not flagged

The rule is replicated exactly from CPython 3.13:

| Shape | Flagged |
|---|---|
| `return` / `break` / `continue` directly in the handler body | yes |
| ... nested in an `if` / `with` / `match` / inner `try` inside the handler | yes |
| `return` inside a `for` / `while` declared in the handler | yes |
| `break` / `continue` bound to a `for` / `while` declared in the handler | **no** |
| `break` / `continue` in that loop's `else:` clause (targets the *outer* loop) | yes |
| `return` inside a nested `def` / `class` in the handler | **no** |
| Anything in the `try` body, the `else:` clause, or the `finally:` clause | **no** |
| Any of the three inside a plain `except` (no `*`) | **no** |

## Fix

Record the outcome in the handler and act on it after the `try` statement:

```ty
def handle() -> int:
    mut code: int = 0
    try:
        raise ExceptionGroup("g", [ValueError("bad")])
    except* ValueError:
        code = 1
    return code
```

```ty
def scan(items: list[str]) -> None:
    for item in items:
        mut failed: bool = False
        try:
            check(item)
        except* ValueError:
            failed = True
        if failed:
            continue
        report(item)
```

If the code does not actually need exception-group splitting — it is catching
one exception from one call, not a partial failure of a `gather:` fan-out —
use a plain `except`, where `return` / `break` / `continue` are all legal:

```ty
def handle() -> int:
    try:
        raise ValueError("bad")
    except ValueError:
        return 1                       # ok: plain `except`
    return 0
```

## Related

`except*` is the only CPython-correct way to handle a failure inside a
`gather:` block: `asyncio.TaskGroup` re-raises child failures wrapped in an
`ExceptionGroup`, so a surrounding `except ValueError:` does not match. See
`docs/vm.md` and the `gather:` section of `docs/language.md`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/return_in_except_star.md
