# tyc::resource_not_managed

Fires when a call to a known resource-returning function (`open`,
`socket.socket`, `sqlite3.connect`, `tempfile.NamedTemporaryFile`, …)
is bound to a variable without a surrounding `with` statement. Without
`with`, the handle is only released when the garbage collector runs —
non-deterministic at best, and lost entirely if an exception escapes
before the binding falls out of scope.

## Example

```ty
def load_config(path: str) -> str:
    let f = open(path)        # warning: returns a resource that should be managed by `with`
    return f.read()
```

## Why

Python's GC will eventually close an unmanaged file handle, but
"eventually" is the wrong word in production: long-running services
leak file descriptors and exhaust the per-process limit; sockets stay
in `TIME_WAIT` past when they should have been released; database
connections silently hold transactions open.

`with` makes cleanup deterministic and exception-safe — the file
closes when the block exits, no matter how it exits. The check fires
on the same callees that the standard library documents as context
managers, so the suggested fix is always "wrap it in `with`".

## Fix

```ty
def load_config(path: str) -> str:
    with open(path) as f:
        return f.read()
```

If you genuinely need the handle to outlive its construction site
(returning it from a factory, stashing it in a class field), wrap the
call in an `unsafe:` region to acknowledge the deferred-cleanup
contract:

```ty
def make_log() -> TextIO:
    unsafe:
        return open("log.txt", "a")
```

## Configuration

Severity is controlled by `[strictness] require-with` in `typhon.toml`:

```toml
[strictness]
require-with = "warn"    # default — visible but doesn't break CI
# require-with = "error" # promote to a hard error
# require-with = "off"   # drop the diagnostic entirely
```

## Coverage

The current registry of resource-returning callees:

- `open`
- `socket.socket`
- `sqlite3.connect`
- `tempfile.NamedTemporaryFile`
- `tempfile.TemporaryDirectory`
- `tempfile.TemporaryFile`

Project-specific classes can opt in via a `.dty` annotation in a
future release.

See https://typhon.dev/lang/diagnostics/resource_not_managed
