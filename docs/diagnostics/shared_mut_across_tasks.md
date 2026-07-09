# tyc::shared_mut_across_tasks

Advice-level diagnostic, **on by default** — but only fires when the project
targets free-threaded Python (`[python] free-threaded = true`). Surfaces a
`go`-spawned same-module function that writes module-level mutable state: a
`global NAME` assignment, or an assignment / augmented-assignment to a
module-level `mut` binding.

Under free-threaded Python a `go`-spawned task runs *concurrently* with the code
that spawned it (and with every other task), so an unguarded write to shared
state is a data race — lost updates, torn reads, or corrupt containers.

Gated by `[strictness] suggest-parallel` (default `true`). Because the example /
stress corpus never sets `free-threaded`, this lint is silent across it by
construction.

## Example

```ty
# [python] free-threaded = true
mut hits: int = 0

async def record() -> None:
    global hits
    hits = hits + 1          # concurrent write to shared mutable state

async def serve() -> None:
    go record()              # advice: `record` runs concurrently with the spawner
```

The `go record()` spawn and the spawner both touch `hits` with no
synchronisation. On a free-threaded build the two `hits = hits + 1` executions
can interleave and lose an update.

## Why

`go f(...)` lowers to a strong-ref background task
(`typhon_runtime.tasks.spawn`). With the GIL, most such writes were
*accidentally* atomic; free-threaded Python removes that safety net, so a
previously-"fine" fire-and-forget mutation becomes a race.

The lint is conservative — it only fires for a **bare-name callee** that
resolves to a same-module `def` and that *directly* writes a `global` or a
module-level `mut` binding. A `go obj.method()` (attribute callee) or a callee
whose shared writes are indirect is not flagged.

## Fix

Guard the shared state, or restructure so the task doesn't share it:

```ty
import asyncio

freeze let LOCK = asyncio.Lock()
mut hits: int = 0

async def record() -> None:
    global hits
    async with LOCK:
        hits = hits + 1
```

or have the task **return** its result and let the spawner (or a queue)
accumulate it instead of writing a global. Silence the nudge project-wide with
`[strictness] suggest-parallel = false`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/shared_mut_across_tasks.md
