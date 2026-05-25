# 10-distributed-kv — friction log

New friction observed while writing the Raft-lite KV app, beyond the
twelve issues in `examples/apps/TYPHON_FEEDBACK.md`. Issues from
Round 1 that I hit *again* but already had a workaround for are noted
at the bottom and not re-litigated.

## 1. Newtype arithmetic forces `int(...)` ping-pong everywhere (severity: HIGH)

Code that broke / felt awkward:
```ty
# `LogIndex` and `Term` are both `newtype X = int`. Any arithmetic
# between them — or with a raw int — requires unwrap-then-rewrap.
let new_idx: LogIndex = self.log.last_index() + 1                # ❌ no `+` between LogIndex and int
let majority: int = (len(self.peers) // 2) + 1
mut n: int = int(last_idx)
while n > int(self.commit_index):                                # both unwraps just to compare
    ...
self.commit_index = LogIndex(n)                                  # rewrap to assign
```

Workaround applied:
```ty
let new_idx: LogIndex = LogIndex(int(self.log.last_index()) + 1)
let next_idx: int = int(self.last_applied) + 1
self.last_applied = LogIndex(next_idx)
```

Why this is a weakness: a `newtype X = int` is meant to give you a
distinct nominal type, but `Term`, `LogIndex`, `NodeId`, and
`ClientReqId` all share the same underlying `int` representation and
every interesting algorithm (commit-index advance, log-index
comparison, term comparison) is arithmetic. The current rules force a
constant `int(x)` … `LogIndex(y)` dance that drowns the actual logic.
At minimum, same-newtype `+`, `-`, `<`, `<=`, `>`, `>=`, `==` should
be allowed; ideally `newtype X = int` arithmetic with raw `int`
literals should be allowed in one direction.

## 2. `dict[int, X]` is the de-facto map type because newtype keys don't work as dict keys (severity: MEDIUM)

Code that broke / felt awkward:
```ty
# What I wanted:
mut next_index: dict[NodeId, int] = {}
next_index[peer] = 1

# What I had to write:
mut next_index: dict[int, int] = {}
next_index[int(peer)] = 1
let ni: int = next_index[int(peer)] if int(peer) in next_index else 1
```

Workaround applied: every dict in `Node` and `Cluster` uses raw `int`
keys and the code wraps/unwraps the newtype at every access:
- `Cluster.nodes: dict[int, Node]` instead of `dict[NodeId, Node]`
- `Node.pending_client: dict[int, ClientReqId]` instead of `dict[LogIndex, ClientReqId]`
- `RoleLeader.next_index: dict[int, int]` instead of `dict[NodeId, LogIndex]`
- `RoleCandidate.votes: dict[int, bool]` instead of `dict[NodeId, bool]`

Why this is a weakness: I chose `dict[int, X]` everywhere rather than
fight this, but it defeats half the point of the newtype — IDs leak
back to bare `int` at every container boundary. Either `dict` should
accept newtype keys natively (the emitted Python is identical) or the
docs should call this out and recommend a workaround.

## 3. Re-emitting a sealed-union variant from a `match` arm requires reconstruction or per-variant helpers (severity: HIGH)

Code that broke / felt awkward:
```ty
def handle(self, msg: Message, now: float) -> list[tuple[int, Message]]:
    match msg:
        case MsgAppendEntries(...) as ae:           # ❌ I wasn't sure `as` was supported
            return self._on_append(ae, now)
        # ...
```

Workaround applied: split every variant into its destructured fields,
pass *all* of them as separate parameters to a typed helper. The
`handle` method ended up being 28 lines of pure unpacking-and-forwarding
because the alternative (passing the matched variant value itself, or
reconstructing the variant) hits the cross-module variant→union upcast
issue from Round 1 #1 in the opposite direction (you cannot use a
variant value where the union is expected, AND you cannot pass the
sub-variant typed as itself because the helper would need to import
the variant and re-export it).

```ty
case MsgAppendEntries(ae_term, ae_leader, ae_pi, ae_pt, ae_ents, ae_lc):
    return self._on_append(
        ae_term, ae_leader, ae_pi, ae_pt, ae_ents, ae_lc, now
    )
case MsgAppendEntriesReply(aer_term, aer_from, aer_ok, aer_mi):
    return self._on_append_reply(aer_term, aer_from, aer_ok, aer_mi, now)
# ... etc for 7 variants
```

Why this is a weakness: when you match a sealed union, every arm has a
*known* concrete variant type. A `case Variant(...) as v:` binding (or
just allowing `case Variant(...): self._helper(msg_typed_as_Variant)`)
would let me write per-variant handlers that take the variant directly
and skip the per-field tuple unpacking. The 7-variant `Message`
dispatch suffered the most — `_on_append` takes six positional
arguments only because handing it `MsgAppendEntries` cleanly isn't
supported.

## 4. `mut` field of a class is sometimes hard to rebind to a fresh variant of a sealed-union field (severity: MEDIUM)

Code that broke / felt awkward:
```ty
# Inside impl Node, with `role: Role` declared on the class.
# Goal: bump only the heartbeat deadline of the *current* RoleLeader.
self.role.heartbeat_deadline = next_hb   # ❌ Role is a union, has no such attr
```

Workaround applied: destructure the variant, then rebuild it via the
factory:
```ty
def _update_heartbeat_deadline(self, new_deadline: float) -> None:
    match self.role:
        case RoleLeader(ni, mi, _):
            self.role = make_leader(ni, mi, new_deadline)
        case RoleFollower(_, _):
            pass
        case RoleCandidate(_, _):
            pass
```

Why this is a weakness: this is a real Raft pattern — leader bumps
heartbeat without touching `next_index`/`match_index` — but in Typhon
you cannot do a *partial* update of a variant when the field is typed
as the union. You have to fully reconstruct via the factory, which
in turn requires the factory to exist in the union's module. Combined
with friction #3 above, role-state transitions are 3× the LoC they
should be.

## 5. `mut` local variable inside a `match` arm causes friction even with per-arm naming (severity: LOW)

Code that broke / felt awkward:
```ty
match self.role:
    case RoleLeader(_, match_index, _):
        ...
        mut count: int = 1
        for v in match_index.values():
            if v >= n:
                count = count + 1
```

This actually parsed and (likely) checks, but the discomfort is that
the Round 1 finding about `let` shadowing sibling arms is *also* true
for `mut`. I avoided naming `count` twice across arms, but the
relevant inner-arm `mut` is fine in isolation. The friction is: I
keep second-guessing whether a per-arm `mut` will trigger the same
no-block-shadow rule as Round 1 #6. The diagnostic should explicitly
permit (or at least document) per-arm `mut`/`let` if it differs from
the sibling-arm scoping rule for `let`.

## 6. Tuple-typed return values don't infer cleanly when assembled inline (severity: MEDIUM)

Code that broke / felt awkward:
```ty
# I expected:
out.append((peer, ae))                     # ❌ peer is NodeId, but tuple is list[tuple[int, Message]]

# Forced to spell out the int wrap at every push:
out.append((int(peer), rv))
out.append((int(leader), reply_stale))
out.append((CLIENT_RECIPIENT, reply_redir))
```

Workaround applied: every tuple constructor along the
`list[tuple[int, Message]]` carrier path manually unwraps `NodeId`
with `int(...)` — there are 17 such sites in `node.ty`.

Why this is a weakness: declaring the carrier as
`list[tuple[NodeId, Message]]` instead would *not* fix this either,
because the special `CLIENT_RECIPIENT = -1` sentinel isn't a `NodeId`.
This is the well-known "union of bare-int and newtype" problem in a
new costume. A `Recipient = NodeId | ClientSentinel` union would
escape it, but at the cost of yet another sealed-union dispatch on
every delivery. Either path is awkward.

## 7. `pub let CONST: int = -1` (top-level immutable constant) parses but doesn't compose with `freeze` (severity: LOW)

Code that broke / felt awkward:
```ty
pub freeze let CLIENT_RECIPIENT: int = -1   # ❌ tyc::parse (already in Round 1 #3)
pub let CLIENT_RECIPIENT: int = -1          # ✅ works (not deep-immutable but OK for an int)
```

Workaround applied: use `pub let`. For a plain `int` the semantic gap
is invisible, but this is the same Round 1 #3 issue with a fresh
example.

Why this is a weakness: see Round 1 #3.

---

## Round 1 issues encountered again

- **Cross-module variant→union upcast (#1)**: factory functions in
  `rpc.ty` (`make_append_entries`, `make_append_entries_reply`,
  `make_request_vote`, `make_request_vote_reply`, `make_client_put`,
  `make_client_get`, `make_client_reply` — 7 factories) and in
  `log.ty` (`make_cmd_set`, `make_cmd_delete`, `make_cmd_noop`) and in
  `state.ty` (`make_follower`, `make_candidate`, `make_leader`). 13
  factories solely to satisfy the upcast restriction.
- **Exhaustive match + missing-return (#5)**: 11 `raise RuntimeError("unreachable")`
  trailers across `log.ty`, `rpc.ty`, `state.ty`, `node.ty`.
- **`?` cross-module params (#4)**: `become_follower(term, None, now)`
  is called several times with a literal `None`. I bound `let voted: NodeId? = self.voted_for`
  before narrowing in `_on_request_vote` rather than calling
  `if self.voted_for is not None and int(self.voted_for) != ...` —
  the latter pattern was a candidate-for-bug from Round 1 #4.
- **`pub freeze let` (#3)**: see new finding #7 above.
- **Pattern arity must match field count exactly (#9)**: every
  `case MsgX(...)` and `case CmdX(...)` and `case RoleX(...)` spelled
  out every field including the ones it ignored with `_`.
