# 07-game-ecs

A small in-process Entity-Component-System game engine, written in Typhon.
It spawns entities, runs a prioritised pipeline of systems (movement,
collision, damage, despawn, tick-end) over many ticks, and emits a
typed game event stream. No graphics; this is a pure simulation chosen
to stress Typhon's sealed unions, dense `match`, frozen value classes,
`Callable`-based dispatch (a workaround for the cross-module
`interface` friction), and a hand-rolled per-kind component store.

## Run

```bash
cd examples/apps/07-game-ecs
tyc check src/
tyc build
python build/main.py
```

## Typhon features exercised

- `pub newtype` for distinct id and name types (`EntityId`, `SystemName`)
- `pub class ... frozen` for value-shaped components and events
- Mutable `class` for `Health` and the central `World` store
- `pub type` sealed unions for `Component` and `GameEvent` with full
  exhaustive `match` over every variant
- Factory functions in each union's defining module (workaround for
  the cross-module variant-to-union upcast friction)
- `Callable[[World, float], None]` system signatures, registered into
  a `Scheduler` (workaround for cross-module `interface` friction)
- `impl` blocks for `World` and `Scheduler`
- Nullable component accessors (`Position?`, `Velocity?`, ...) with
  `if x is not None` narrowing at every call site
- `raise RuntimeError("unreachable")` after every exhaustive match,
  to silence `tyc::missing_return`
