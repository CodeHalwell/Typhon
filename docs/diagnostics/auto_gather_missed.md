# tyc::auto_gather_missed

Advice-level diagnostic. Surfaces when two or more adjacent `await CALLEE(...)`
statements look independent enough to fold into an `asyncio.TaskGroup`, but
at least one callee is a same-module `async def` that isn't decorated
`@gatherable`. The auto-gather pass only rewrites runs where every callee
opts in.

## Example

```ty
async def fetch_a() -> int: ...
@gatherable
async def fetch_b() -> int: ...

async def main() -> None:
    let a = await fetch_a()  # advice: fetch_a is not @gatherable
    let b = await fetch_b()
```

## Why

`@gatherable` is the user's attestation that a function is safe to run
concurrently with peers (no shared mutable state, no ordering dependency).
Without it, the auto-gather pass keeps the awaits sequential — silently.
Surfacing the missed opportunity nudges users to opt in deliberately rather
than discover the leftover latency in production.

## Fix

Decorate every same-module async callee in the run with `@gatherable`:

```ty
@gatherable
async def fetch_a() -> int: ...
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/auto_gather_missed.md
