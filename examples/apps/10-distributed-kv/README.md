# 10-distributed-kv — Raft-lite key-value store

A single-process simulation of a three-node distributed key-value store
loosely inspired by Raft. Nodes exchange messages via an in-memory
message bus that the cluster steps deterministically by clock tick.
Leader election uses term numbers and majority voting; log replication
uses `AppendEntries` with consistency checks; commands commit once
replicated to a majority and only then update the key-value store.

This is *not* a faithful Raft implementation — there are no real
network partitions, no snapshotting, no membership changes, and the
election randomisation is deterministic (derived from node id) so the
simulation is reproducible. The point is to stress Typhon with a
large sealed-union message protocol, role state machines, and a lot
of pattern matching.

## Run

```bash
cd examples/apps/10-distributed-kv
tyc check src/
tyc build
python build/main.py
```

## Typhon features exercised

- Large 7-variant sealed union for inter-node + client RPC messages.
- Sealed-union role state (`RoleFollower | RoleCandidate | RoleLeader`)
  driving every node's behaviour through exhaustive `match`.
- Per-variant factory functions in the message module to work around
  the cross-module variant-to-union upcast restriction.
- Four `newtype`s (`NodeId`, `Term`, `LogIndex`, `ClientReqId`) used
  throughout for stronger keys; arithmetic done via plain `int` and
  rewrapped at the boundary.
- `Result`-free design: the simulator distinguishes failure paths via
  `MsgClientReply.ok` rather than `Result[T, E]`, since RPC replies
  are themselves messages.
- Deterministic single-threaded scheduling via a `Cluster.step(dt)`
  loop — no `asyncio`, no `gather:`, no `go`.
- `mut` fields on `Node` mutated inside `impl` methods as the role
  machine transitions between follower / candidate / leader.
