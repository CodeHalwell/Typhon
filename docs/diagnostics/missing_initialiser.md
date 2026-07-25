# tyc::missing_initialiser

**Severity: warning.** Fires when a declare-only `let NAME: T` (or
`mut NAME: T`) is never assigned on any path **and** never read — a dead
binding. It emits a bare `NAME: T` annotation into the generated Python,
which does nothing at runtime, so this flags dead code rather than rejecting
a working program. It never fails a build.

## Example

```ty
def start() -> str:
    let port: int        # ⚠ tyc::missing_initialiser — never assigned, never read
    return "started"
```

## Why

Declare-then-assign became a supported form in **v0.7.0**, when the resolver
gained definite-assignment analysis: a declare-only `let` is legal, and the
*first* subsequent assignment is its initialiser.

```ty
def parse(raw: str) -> Result[Cfg, str]:
    let loaded: Cfg               # ✅ declare only
    match _load(raw):
        case Ok(v):  loaded = v   # this assignment IS the initialiser
        case Err(e): return Err(e)
    return Ok(loaded)
```

That left four shapes, three of which were already policed:

| Shape | Diagnostic |
|---|---|
| declared, then assigned | accepted — the assignment is the initialiser |
| read on a path that never assigned it | [`tyc::use_of_uninitialised`](./use_of_uninitialised.md) (error) |
| assigned a **second** time | [`tyc::immutable_assign`](./immutable_assign.md) (error) |
| never assigned **and** never read | `tyc::missing_initialiser` (warning) |

The last row was silently accepted until this lint was wired up. It is the
only one of the four where the program runs correctly, which is why it is a
**warning**: Typhon's compatibility rule is that a change may only reject
code that was already broken, never narrow a program that ran correctly.

## Detection

The resolver already tracks every declare-only `let` / `mut` declaration
span so the first assignment can claim it as the initialiser. Any span that
survives the whole walk was never assigned. Of those, the ones whose name is
never referenced anywhere in the module are reported.

The "never read" test matches on the **name across the whole module**, so any
mention at all — a read, an augmented assignment, an f-string interpolation —
silences the lint. That is deliberately the conservative direction: the check
can only ever under-report, never fire on a binding that is genuinely in use.

## Fix

Give the binding a value at its declaration:

```ty
def start() -> str:
    let port: int = 8080
    return f"started on {port}"
```

…assign it before it is read:

```ty
def start(dev: bool) -> str:
    let port: int
    if dev:
        port = 3000
    else:
        port = 8080
    return f"started on {port}"
```

…or, when nothing ever uses it, delete the declaration:

```ty
def start() -> str:
    return "started"
```

## Related

- **`tyc::use_of_uninitialised`** — the binding is read on a path that never
  assigned it. That one catches a real runtime `NameError`, so it is an error.
- **`tyc::immutable_assign`** — a second assignment to a declare-only `let`.
- **`tyc::unused_import`** — the same "declared but never used" idea applied
  to imports.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/missing_initialiser.md
