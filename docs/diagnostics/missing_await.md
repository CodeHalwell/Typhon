# tyc::missing_await

Fires when an `async def` is called without `await` in a position where the
coroutine object cannot be what the program meant. The expression's value is a
coroutine, not the function's declared return type, and Python's runtime emits
"coroutine was never awaited" warnings for these.

There are two shapes.

## Shape 1 — a sync caller

A synchronous function (or module scope) calling an `async def` at all: there
is nowhere for the coroutine to be awaited, so it is always a bug.

```ty
async def fetch() -> int:
    return 42

def main() -> int:
    return fetch()  # error: missing `await` on async call to `fetch`
```

## Shape 2 — an async caller binding into a return-typed slot

Since **v1.0.0-alpha.7**, the diagnostic also fires inside another `async def`
— the likelier mistake, since that is where await-heavy code lives. Before
that release this case was completely silent, and `tyc::async_without_await`
only covered for it by accident (when the caller happened to have no other
`await`).

```ty
import asyncio

async def fetch_a() -> dict[str, int]:
    await asyncio.sleep(0)
    return {"a": 1}

async def run() -> int:
    let b: int = await fetch_b()
    let d: dict[str, int] = fetch_a()  # error: missing `await` on async call to `fetch_a`
    return len(d) + b
```

The emitted Python dies with `TypeError: object of type 'coroutine' has no
len()` plus `RuntimeWarning: coroutine 'fetch_a' was never awaited`.

The rule here is deliberately narrow: it fires **only when the un-awaited call
flows into a slot annotated with the coroutine's own return type** — the
annotation of a `let` / annotated assignment, or the enclosing function's
declared return type at `return f()`. That shape is never a deliberate
deferral, because a coroutine you mean to await later is never annotated with
the value it will eventually produce.

### What stays legal

Binding a coroutine now and awaiting it later is an intentional, supported
pattern, and none of these fire:

```ty
let task = fetch_a()                                  # no annotation
let data: dict[str, int] = await task

go fetch_a() -> handle                                # `go` spawns
let r: dict[str, int] = await handle

gather:                                               # TaskGroup lowering
    a = fetch_a()
    b = fetch_b()

let t = asyncio.create_task(fetch_a())                # coroutine as an argument
let rs = await asyncio.gather(fetch_a(), fetch_b())

let anything: object = fetch_a()                      # widening slot; a
                                                      # coroutine *is* an object
```

The rule requires type *equality* between the annotation and the callee's
return type, not assignability, which is what keeps the `object` case quiet.

## Why

Calling an `async def` produces a coroutine; the body only runs when the
coroutine is awaited (or scheduled by an event loop). Treating the coroutine as
if it were the declared return value silently propagates a wrong-typed value
across the program.

## Fix

Add `await`:

```ty
async def run() -> int:
    let b: int = await fetch_b()
    let d: dict[str, int] = await fetch_a()
    return len(d) + b
```

From a sync caller, make the caller `async` or use `asyncio.run(...)` at the
top level:

```ty
import asyncio

async def main_async() -> int:
    return await fetch()

def main() -> int:
    return asyncio.run(main_async())
```

If you genuinely meant to start the coroutine now and await it later, drop the
return-type annotation from the binding (`let task = fetch_a()`), or hand the
coroutine to `asyncio.create_task(...)` / `asyncio.gather(...)` / `go f(...)`.

## Limits

The check keys on module-level `async def` names declared in the file being
checked. An un-awaited call to an `async` *method*, or to an async function
imported from another module, is not covered.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/missing_await.md
