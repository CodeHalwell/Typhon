# 11 — Realtime multiplayer game server

A production-shaped multiplayer game server with:

- **Lobby system** — players join, get matchmade into rooms by ELO skill
- **Room state machine** — `Waiting → Playing → Resolving → Closed`, all sealed-union
- **Async tick loop** per active room (30 Hz simulation), broadcasting state diffs to per-room subscribers
- **Pluggable game modes** — `duel` (head-to-head) + `battle_royale` (free-for-all), both as concrete strategy structs holding `Callable` fields
- **ELO ratings** — newtype `Rating = int`, free-for-all rank-to-score mapping, K-factor frozen as a config constant
- **Connection registry** — `SessionToken` -> `Session` with TTL, resume-by-token reconnect, single-session-per-player invariant
- **Persistent player profiles + match log** via SQLite (idempotent upsert, leaderboard query, per-player history)
- **Async control plane** — FastAPI endpoints to register a player, queue for a mode, query profiles/rooms/leaderboard/history, stream events
- **Structured event log** — every transition emits a sealed-union `GameEvent` with module-local factory functions

## Files

| File | Responsibility |
|---|---|
| `src/domain/ids.ty` | `newtype` wrappers (`PlayerId`, `RoomId`, `LobbyId`, `Rating`, `SessionToken`, `MatchId`) |
| `src/boot/config.ty` | `freeze let` constants (tick rate, K-factor, room sizes) + `comptime let GAME_VERSION` |
| `src/domain/elo.ty` | ELO calculation; expected score, head-to-head update, free-for-all rank distribution |
| `src/runtime/buffer.ty` | Generic `RingBuffer[T]` used for per-room event history |
| `src/domain/events.ty` | Sealed `GameEvent` union + factory functions + label/room/player accessors |
| `src/domain/profiles.ty` | SQLite-backed `ProfileStore` with `Result[T, ProfileError]` returns + match persistence |
| `src/runtime/registry.ty` | `ConnectionRegistry` with `SessionToken` issuance, resume, and TTL eviction |
| `src/domain/modes.ty` | `Mode` strategy struct + `duel`/`battle_royale` concrete implementations |
| `src/runtime/rooms.ty` | `Room` + sealed `RoomState` state machine + `room_tick_loop` async runner |
| `src/runtime/lobby.ty` | Matchmaking by rating, room spawning, finalise queue, async loops |
| `src/transport/api.ty` | FastAPI endpoints (`/players/register`, `/lobby/queue`, `/rooms`, …) + Pydantic `model` types |
| `src/main.ty` | Wires everything together, spawns lobby loops via `go`, runs uvicorn |

## Features exercised

- `newtype` IDs for `PlayerId`, `RoomId`, `LobbyId`, `Rating`, `SessionToken`, `MatchId`
- `freeze let` for tick rate, ELO K-factor, max room players, room timeout, session TTL
- `comptime let GAME_VERSION` driven by build-time env (`env("GAME_VERSION", "0.1.0-dev")`)
- Sealed `GameEvent` union (7 variants) with module-local factory functions (Round-1 #1 workaround)
- Sealed `RoomState` state machine (4 variants) driven by `impl Room: def advance_tick()` etc.
- Sealed `SessionError`, `ProfileError`, `RoomError` unions (multiple distinct `E` types for `Result[T, E]`)
- Generic `RingBuffer[T]` class with `impl[T] RingBuffer[T]:` methods
- `gather:` block in `api.warm_state()` for parallel SQLite-via-`asyncio.to_thread` reads
- `go room_tick_loop(...)` spawned per matchmade room; `go lobby.matchmake_loop()` for the per-second matchmaker
- `Result[T, E]` with two distinct `E` types (`StoreError`, `ProfileError`, `RoomError`, `SessionError`)
- Exhaustive `match` over `RoomState` (4 arms), `GameEvent` (7 arms), `Role`-like patterns — every match closes with `raise RuntimeError("unreachable")` (Round-1 #5 workaround)
- `impl Room`, `impl Lobby`, `impl ConnectionRegistry`, `impl ProfileStore` for state-machine methods
- `pub` markers on every public symbol that crosses a module boundary
- FastAPI endpoints with Pydantic `model` request/response types
- `Mode` as a concrete struct holding `Callable` fields, *not* an interface (Round-1 #2 workaround)

## Running

```bash
cd examples/apps/11-realtime-game-server
tyc check src/
tyc build
python build/main.py
# in another shell:
curl -X POST localhost:8000/players/register \
     -H 'content-type: application/json' \
     -d '{"name":"alice"}'
# {"player_id":1,"name":"alice","rating":1000,"session_token":"…"}

curl -X POST localhost:8000/lobby/queue \
     -H 'content-type: application/json' \
     -d '{"session_token":"…","mode":"duel"}'

curl localhost:8000/leaderboard
curl localhost:8000/rooms
curl localhost:8000/events?since=0
curl localhost:8000/matches/recent
curl localhost:8000/players/1/history
```
