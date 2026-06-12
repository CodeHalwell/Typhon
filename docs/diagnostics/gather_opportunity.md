# tyc::gather_opportunity

Advice-level diagnostic, **on by default**. Surfaces when two or more adjacent
`NAME = await CALL(...)` statements inside an `async def` are independent by
data flow — no later await consumes a name bound by an earlier one — so they
run sequentially when they could run concurrently.

Unlike [`tyc::auto_gather_missed`](auto_gather_missed.md), this is
*callee-agnostic*: it fires for awaited **method calls on imported clients**
(`await client.get_user(id)` then `await client.get_posts(id)`) — the most
common real missed-concurrency shape — not just bare-name calls to same-module
`async def`s. The suggested fix is the explicit `gather:` block, which works
for any awaitable with no `@gatherable` decorator or `auto-gather` opt-in.

## Example

```ty
async def load(client: Client, uid: int) -> tuple[User, list[Post]]:
    let user: User = await client.get_user(uid)       # advice: these 2 awaits
    let posts: list[Post] = await client.get_posts(uid)  # could run concurrently
    return (user, posts)
```

## Why

The two awaits don't depend on each other, so awaiting them one after another
spends the sum of their latencies when it could spend the maximum. For
I/O-bound calls (HTTP, database, RPC) that is often a 2× or larger latency win
for free.

The diagnostic is **advice**, never an error, and the compiler never rewrites
the code: running awaits concurrently is a behaviour change the author must opt
into, because data-flow independence does not rule out ordering side effects
(two writes to the same backend, rate limits, etc.).

## Fix

Wrap the run in a `gather:` block (lowers to an `asyncio.TaskGroup`):

```ty
async def load(client: Client, uid: int) -> tuple[User, list[Post]]:
    gather:
        user = client.get_user(uid)
        posts = client.get_posts(uid)
    return (user, posts)
```

If the awaits must stay ordered (shared state, side-effect ordering), leave them
sequential — and silence the nudge project-wide with `[strictness]
suggest-gather = false`.

See https://typhon.dev/lang/diagnostics/gather_opportunity
