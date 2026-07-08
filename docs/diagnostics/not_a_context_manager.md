# tyc::not_a_context_manager

Fires when the subject of a `with` (or `async with`) is a locally-defined class
that does not implement the context-manager protocol.

## Example

```ty
class Plain:
    label: str

impl Plain:
    def greet(self) -> str:
        return "hi " + self.label

def main() -> None:
    with Plain(label="world") as p:   # error: Plain has no __enter__
        print(p.greet())
```

`async with` requires the async protocol:

```ty
class NotAsync:
    pass

impl NotAsync:
    def __enter__(self) -> NotAsync:
        return self
    def __exit__(self, a: object, b: object, c: object) -> None:
        return None

async def main() -> None:
    async with NotAsync() as m:        # error: NotAsync has no __aenter__
        print("got", m)
```

## Why

A `with EXPR as v` block calls `EXPR.__enter__()` on entry and
`EXPR.__exit__(...)` on exit; `async with` calls `__aenter__`/`__aexit__`.
A subject lacking those methods raises `TypeError` at the `with` statement:

```
TypeError: 'Plain' object does not support the context manager protocol
```

The program type-checked clean but was guaranteed to crash, so Typhon rejects
it up front.

The check is conservative: it only fires for a class whose entire ancestry is
defined in the current module (so every method is known) and that definitely
lacks the protocol. A stdlib/third-party context manager (`open(...)`,
`asyncio.Lock`, a database session), a subject of unknown type, and a
`@contextmanager` / `@asynccontextmanager` factory all stay permissive.

## Fix

Implement the protocol, or use a `@contextmanager` factory:

```ty
import contextlib
from collections.abc import Iterator

@contextlib.contextmanager
def plain(label: str) -> Iterator[str]:
    yield label

def main() -> None:
    with plain("world") as p:          # ok: factory provides the protocol
        print(p)
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/not_a_context_manager.md
