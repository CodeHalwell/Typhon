# Typhon — feedback from building the five `examples/apps/`

Notes captured while writing five multi-file production-shaped apps under
`examples/apps/`. Each section is a friction point that cost real time
(or workarounds that bloat the code), ordered by impact. Tested against
tyc 0.5.2 against a fresh build of the workspace.

## 1. Cross-module sealed-union variant → union upcasting (HIGH IMPACT)

**The single biggest source of errors** in the apps (≈ 60 of the 103
initial errors). Within the module that defines the union, a variant
can be passed where the union is expected. Across module boundaries,
it cannot.

```ty
# lib.ty
pub type Event = A | B
pub class A: x: int
pub class B: y: str

# main.ty
from lib import A, Event
def emit(e: Event) -> None: ...
emit(A(x=1))                    # ❌ tyc::type_mismatch — expected `Event`, found `A`

let evt: Event = A(x=1)         # ❌ same error — explicit annotation doesn't help
return A(x=1)                   # ❌ same error — even from a `-> Event` function

# Workarounds I had to apply:
mut events: list[Event] = []
events.append(A(x=1))           # ✅ list[Union].append(Variant) DOES work
```

The asymmetry is the worst part. The only patterns that work cross-module today:
1. `list[Union].append(Variant)`
2. A factory function **defined in the union's own module** that wraps construction with an explicit `-> Union` return.

So every event/state/command in the five apps grew a `make_xxx() -> Union`
factory layer in the module where the union lives, just to bridge the
gap. The banking app needed ~22 factories (8 commands + 8 events +
8 rejections). The scheduler needed 14. That's a lot of boilerplate
for what should be a one-line construction.

**Suggested fix:** when a class is a declared variant of a sealed union
visible at the import site (i.e. when both the variant *and* the union
type are imported, or when the union is reachable from the target
type), allow the variant to flow into a position typed as the union —
either in function arguments, in return statements, or in `let`
annotations. The information is already in the type DB; this is a
checker rule, not a semantics change.

## 2. Interface conformance is opaque across modules (HIGH)

The same issue as #1 but for `interface`. A concrete class with all
the right methods can satisfy an interface defined in its own module
but **not** an interface defined in another module:

```ty
# types.ty
pub interface Greeter:
    def hello(self) -> str

pub class Friend:
    name: str

impl Friend:
    def hello(self) -> str:
        return f"hi {self.name}"

# main.ty
from types import Friend, Greeter
def use(g: Greeter) -> None: print(g.hello())
use(Friend(name="ada"))              # ❌ tyc::type_mismatch — expected `Greeter`, found `Friend`

# Workaround that DOES work:
mut gs: list[Greeter] = []
gs.append(Friend(name="ada"))        # ✅
```

I had to drop the `Handler` interface entirely from the task scheduler
and replace it with a concrete `class Handler { name: str; fn: Callable[...] }`
plus factory functions. That deletes most of the value of having
interfaces — the whole point is structural polymorphism across modules.

**Suggested fix:** since the emitted code uses `Protocol`, structural
conformance is already accepted by mypy/pyright on the emitted side.
The Typhon checker should match.

## 3. `pub` does not stack with `freeze let` (MEDIUM)

```ty
pub freeze let DEFAULT_RETRY: RetryPolicy = ...    # ❌ tyc::parse
freeze let DEFAULT_RETRY: RetryPolicy = ...        # ✅ works (but private)
pub let DEFAULT_RETRY: RetryPolicy = ...           # ✅ works (but not frozen)
```

The `pub` diagnostic doc explicitly lists `pub let X: int = 1` /
`pub mut Y: int = 0` as supported and says `pub` stacks with the
other keyword modifiers (`pub frozen class`, `pub model`, `pub let`).
But `pub freeze let` is rejected at parse time. Either the doc or the
parser is wrong; I'd expect the doc to win and the parser to accept
`pub freeze let`.

## 4. `?` in cross-module function parameters silently mis-resolves (HIGH — looks like a bug)

```ty
# lib.ty
newtype Price = int
pub def takes(p: Price?) -> int:   # signature is `Price?`
    if p is None: return 0
    return int(p)

# main.ty
from lib import Price, takes
def main() -> None:
    let p: Price? = None
    print(takes(p))     # ❌ "value is `? | None` here, where `?` is required"
```

The error message says "where `?` is required" — the function's
declared `Price?` is being seen as `Price`. The diagnostic's `?`
placeholder also hides which type it thinks it needs (clearly a
display bug). This forced me to drop the `Price?` parameter on
`book.crosses` and have callers narrow at every call site. Quite
painful for an API that wants to express "may be a market order
with no price".

If this is a real bug, it's a regression candidate for a test.

## 5. `match` over a sealed union doesn't satisfy "missing return" analysis (MEDIUM)

```ty
def label(s: AccountState) -> str:
    match s:
        case Open():           return "open"
        case Frozen(_):        return "frozen"
        case Closed(_):        return "closed"
        case Uninitialised():  return "uninit"
    # ❌ tyc::missing_return — "may fall off the end without a return value"
```

The `match` covers every variant of the sealed union (exhaustiveness
*does* fire for missing variants, so the checker knows). I had to add
`raise RuntimeError("unreachable")` after at least a dozen exhaustive
matches across the five apps. If exhaustiveness is verified, fall-off
should be statically unreachable and `missing_return` shouldn't fire.

## 6. `let` inside a `case` arm shadows across sibling arms (MEDIUM)

```ty
match event:
    case MoneyDeposited(aid, amount, _, _, _):
        let key: str = str(aid)                # ✅
        ...
    case MoneyWithdrawn(aid, amount, _, _, _):
        let key: str = str(aid)                # ❌ tyc::no_block_shadow
        ...
```

This forces ugly per-arm renaming (`deposit_key`, `withdraw_key`,
`interest_key`, …). Python `match` *does* introduce per-arm scoping
in practice (each arm reaches the next via fallthrough or completion,
sibling arms can't see each other's bindings) — the no-block-shadow
rule could be relaxed for sibling `case` arms specifically.

## 7. `dict.get(k) or default` works but `let x: T = ...` doesn't bind to `T` (MEDIUM)

For example `args.get("name") or "world"` returns `object`, but if
`args: dict[str, object]` the user really wants the value-typed
default behaviour. Today I have to write:

```ty
let raw: object? = args.get("name")
let who: str = str(raw) if raw is not None else "world"
```

Two extra lines for every dict lookup with a default. This is
exacerbated by the fact that Python's `or` short-circuit works fine
at runtime, but the checker is conservative about its type.

## 8. `loop.run_until_complete(coro())` is flagged as missing await (LOW)

Python's `asyncio.AbstractEventLoop.run_until_complete` *is* the
boundary that consumes a coroutine without an explicit `await`. The
`tyc::missing_await` diagnostic doesn't know that. Workaround was to
switch to `asyncio.run(...)`, which lost the ability to install signal
handlers manually. A stdlib carve-out (`loop.run_until_complete`,
`asyncio.run`, `asyncio.gather` already known) would fix this.

## 9. Pattern-match parameter counts must match dataclass field counts exactly (LOW but noisy)

```ty
class TaskStarted:
    task_id: TaskId
    worker: WorkerId
    attempt: int
    at: datetime

match event:
    case TaskStarted(tid, _, _, _):     # must always be exactly 4 patterns
```

This is fine in isolation but means every change to a dataclass's
fields breaks every pattern that references it. A `__match_args__`
opt-in to allow `case TaskStarted(tid=t):` keyword patterns would help
maintenance, and Python's match already supports that syntax. I avoided
it because I wasn't sure Typhon would parse it.

## 10. `unused-import = "error"` default catches type-inferred uses (LOW)

A type like `AmlRejection` imported solely so that `evaluate(...)`'s
declared return type `Result[None, AmlRejection]` makes sense in
context is flagged as unused. The checker is right *technically* —
nothing in the function body lexically mentions `AmlRejection` — but
the import is documentation of the function's error type and the
checker has the info needed to mark it "used by type-inference reach".

## 11. Diagnostic improvements that would have saved real time

- The `tyc::type_mismatch` help text always says "or update the
  annotation to `<found type>`" — useful for primitives, actively
  misleading for sealed unions (the user almost always wants the union,
  not the variant). For variant→union mismatches, suggest "wrap the
  call site in a factory that returns `<union>`" or just say the upcast
  isn't supported cross-module.
- `tyc::nullable_use` displays the type name as a literal `?`
  placeholder ("value is `? | None`", "where `?` is required"). Render
  the actual type name (`Price | None`, `Price`) so the user can read
  the error without context.
- A `tyc::resource_not_managed` for the `@contextmanager`-decorated
  helper that itself opens the resource is a false positive — that
  helper *is* the manager. The check should skip functions decorated
  with `@contextmanager`.

## 12. `tyc explain --list` claims to exist but is undocumented in CLI.md

Every error message hints `try \`tyc explain --list\` to browse all
codes`. The flag isn't in `docs/cli.md`. Either add it to the docs or
remove the hint.

---

## Summary by app

| app | initial errors | root cause | workaround |
|---|---|---|---|
| 01-task-scheduler | 34 | #1, #2, #3, #5 | factory helpers in models.ty, dropped Handler interface |
| 02-trading-engine | 9 | #4, #5, #6 | callers narrow before `crosses()`, `raise unreachable` |
| 03-ml-orchestrator | 2 | #5 | `raise unreachable` |
| 04-event-sourced-banking | 56 | #1, #5, #6 | 22 factory helpers, per-arm rename |
| 05-web-crawler | 2 | #1, #3 | drop union-typed `record(...)` for variant-specific record methods |

After fixes all five `tyc check` clean, `tyc build` clean, and
`python build/main.py` runs end-to-end (using Python 3.13 for PEP 695
`type` statement support).

## What worked really well

The frustrations above are real but worth balancing against what
worked smoothly:

- **`newtype`** — every monetary axis (Money, Price, Qty, Notional) and
  every ID kind got its own newtype. Caught real "I passed a TraderId
  where Symbol was expected" bugs at check time.
- **`freeze let`** for config constants — clean syntax, deep-immutable
  semantics, exactly what I want for "config that should be read-only
  after startup".
- **PEP 695 generics** — `class Stage[I, O]:` + `impl[T] Box[T]:` felt
  natural and worked first try.
- **`Result[T, E]` + `?`** — the `with`-chain inside `_build_pipeline`
  is one of the cleanest sequenced-error-handling patterns I've used
  in any language.
- **`lazy import np = numpy`** — numpy stays out of the import graph
  until a stage actually trains; the proxy is invisible to user code.
- **`go w.loop() -> task`** — fire-and-forget with strong refs;
  the worker pool just works.
- **Exhaustive `match`** — once you accept (5), the exhaustiveness
  checker is a genuine help and caught two missing-variant bugs while
  I was extending the sealed unions.

The language is already pleasant for ~80% of what these apps need. The
issues above are the ~20% where production-shaped multi-file code hits
limits the single-file examples never exercise.

---
---

# Round 2 — five additional apps (06-graphql-server through 10-distributed-kv)

A second batch of five large multi-file apps was built to stress areas
the first batch didn't exercise: typed GraphQL resolvers + a generic
`DataLoader[K, V]`, an entity-component-system game engine, a Pratt
parser + tree-walking interpreter for a tiny expression language, a
BM25 inverted-index search engine, and a 3-node Raft-lite KV store.
The apps total ~5,500 lines of Typhon across 42 `.ty` files. Tested
against tyc 0.5.2 (with the `pub def` parser fix from finding R2-1
applied locally).

All five Round 2 apps now `tyc check` clean (08-mini-compiler keeps
6 `class_attr_shadows_slot` warnings for nullary sealed-union variants —
see finding R2-2), `tyc build` clean, and `python build/main.py` runs
end-to-end. The notes below are issues that *only* appeared once the
new feature combinations were exercised.

## R2-1. `pub def` is invisible to the `?`-operator validator (severity: HIGH — real bug, patched locally)

The preprocess pass `validate_question_ops` tracks the indent depth of
each enclosing function so it can verify that a `?` only appears
inside a function returning `Result[T, E]`. Its pattern match was
`trimmed.starts_with("def ") || trimmed.starts_with("async def ")` —
which silently misses every `pub def f(...) -> Result[T, E]: ...`
function. As a result every cross-module-public function that used `?`
propagation produced a misleading `tyc::invalid_question_op` saying
"`?` operator used at module level" — when the `?` was clearly inside
a `pub def` whose return type was `Result[T, E]`.

Reproduction:

```ty
# query.ty
pub def parse_query(raw: str) -> Result[QueryNode, str]:
    let tokens: list[Lexeme] = _tokenise(raw)
    if len(tokens) == 0:
        return Err("empty query")
    let parser: Parser = Parser(tokens=tokens, pos=0)
    let root: QueryNode = parser.parse_field()?   # ❌ "module level"
    return Ok(root)
```

Round 1 didn't surface this because every Round 1 `?` lived inside a
`def` (private helper) — not a `pub def` — or inside an `impl` method
(which is also `def`-prefixed at desugar). Round 2's resolvers are
all `pub def` so the bug fires hard.

Fix (patched on this branch in
`tyc/crates/tyc-syntax/src/preprocess.rs:2241`):

```rust
let after_pub = trimmed.strip_prefix("pub ").unwrap_or(trimmed);
if after_pub.starts_with("def ") || after_pub.starts_with("async def ") {
    let ret_type = extract_return_type_text(code);
    fn_stack.push((indent_len, ret_type));
}
```

This is the single biggest Round 2 finding: a real one-line bug in the
preprocess validator. All Round 2 apps were unable to `tyc check`
without this fix.

## R2-2. `class_attr_shadows_slot` fires on nullary variants of frozen sealed unions (severity: MEDIUM)

`08-mini-compiler` declares several nullary sealed-union variants
(`TyInt`, `TyFloat`, `TyStr`, `TyBool`, `TyUnit`, `VUnit`) as

```ty
pub class TyInt frozen:
    placeholder: int = 0
```

because `pub class TyInt frozen:` with an empty body / `pass` body
doesn't parse. The `placeholder: int = 0` is meant to provide a
trivial constructor — but `@dataclass(slots=True)` desugars
`placeholder = 0` into a slot descriptor, not a default value, and
the checker fires `tyc::class_attr_shadows_slot` accusing the class
of "reading like a namespace of constants". The diagnostic is right
about the underlying Python semantics but wrong about the intent —
the author wanted a singleton dataclass, not a constants module.

Workarounds attempted (and rejected):
1. `pub class TyInt frozen: pass` — parse error.
2. `pub class TyInt frozen:` (empty body) — parse error.
3. `class TyInt: pass` (no `frozen`) — parses, but exposed-as-mutable.
4. `placeholder: int` (no default) — works but requires `TyInt(0)` at
   every call site.

Cleanest fix would be either to special-case `placeholder: int = 0`
when emitted by the desugarer (suppress the warning when the *user*
spelled the default) or to accept `pub class X frozen: pass` syntactically.

## R2-3. `impl` on a sealed-union type alias is rejected (severity: MEDIUM)

```ty
pub type ResolvedField = FieldUser | FieldPost | ...

impl ResolvedField:                 # ❌ tyc::impl_unknown_class
    def kind(self) -> str:
        match self:
            case FieldUser(_): return "user"
            ...
```

The user wants a "method-on-the-union" form so `field.kind()` works
regardless of variant. Today `impl` only accepts concrete classes, so
the only path is a free function `field_kind(f: ResolvedField) -> str`
called as `field_kind(field)`. This is a real ergonomic gap: every
sealed union ends up with a "label" free function plus a "render"
free function plus a "tag" free function — each a free `def` instead
of a method. Either `impl` should accept a sealed-union alias (lower
to `singledispatch` or a top-level dispatcher) or the docs should
clearly mark this as not-supported.

## R2-4. Module filenames silently collide with the Python stdlib (severity: HIGH)

`08-mini-compiler` originally had `src/types.ty` and `src/ast.ty` —
both perfectly natural names for a compiler project. The compiled
output is `build/types.py` and `build/ast.py`. Running `python
build/main.py` immediately blows up:

```
ImportError: cannot import name 'Ok' from 'typhon_runtime' ...
AttributeError: partially initialized module 'dataclasses' from
  '/usr/lib/python3.13/dataclasses.py' has no attribute 'dataclass'
  (most likely due to a circular import)
```

Because `build/` is on `sys.path` (the runtime entry point lives
there), `dataclasses → re → enum → from types import ...` picks up
the *project's* `types.py`, not the stdlib's. Same for `ast.py`
(used transitively via `inspect`). The user-visible symptom is a
baffling circular-import error pointing at `dataclasses`.

Workaround applied: rename `types.ty` → `lang_types.ty` and
`ast.ty` → `lang_ast.ty`. But this should be a build-time check —
either `tyc build` should warn when a module name shadows a stdlib
module, or the build harness should structure the output so `build/`
isn't on `sys.path` by default. (`build/__main__.py` with a parent
package is one option.)

This is a real production-grade footgun. Any user writing a parser,
typechecker, http library, threading utility, or io abstraction in
Typhon will eventually hit it, and the error message points at
`dataclasses` (which is innocent), not their own module.

Stdlib names known to collide via transitive imports: `types`, `ast`,
`tokens` (close: `token`), `parser` (removed in 3.10 but still
reserved), `string`, `time`, `re`, `json`, `os`, `sys`, `io`,
`logging`, `email`, `http`, `urllib`, `socket`, `random`, `math`,
`enum`, `inspect`, `functools`, `itertools`, `operator`, `pathlib`,
`copy`, `pickle`, `weakref`, `gc`, `array`, `struct`, `select`,
`signal`, `threading`, `queue`, `dataclasses`, `collections`.

## R2-5. Bound-method values populate `Callable[...]` fields without complaint (severity: LOW — actually worked, but unverified)

The `06-graphql-server`'s `DataLoader[K, V]` declares
`batch_fn: Callable[[list[K]], dict[K, V]]`. In `executor.ty`:

```ty
let user_loader: DataLoader[int, User] = new_loader(store.batch_load_users)
```

Where `store.batch_load_users` is a bound method of `Store`. The
checker accepts it, the runtime works, the emitted Python is clean.
But Round 1 never exercised the bound-method-as-Callable form — every
`Callable` field was populated by a top-level `def`. Worth a regression
test so this doesn't quietly regress in a future release.

## R2-6. Heterogeneous-error `with`-chains have no clean shape (severity: MEDIUM)

`08-mini-compiler`'s natural top-level pipeline is

```ty
with toks   = tokenize(src)?,           # Result[..., LexError]
     ast    = parse(toks)?,             # Result[..., ParseError]
     ty     = check(ast, env)?,         # Result[..., TypeError]
     value  = eval_program(ast)?:       # Result[..., EvalError]
    return Ok((ty_label(ty), value_to_str(value)))
```

But `with`-chains require every `?` to propagate into the same `E`,
and these four functions return four different error types. The
workaround is a 4-deep `match` tower with 4 `raise RuntimeError("unreachable")`
trailers — i.e. the natural shape becomes ~25 lines instead of 6.

Round 1 didn't see this because each Round 1 pipeline threaded a single
error type (`PipelineError` across all stages of the ML orchestrator,
for example). Real compilers / ETL pipelines / multi-stage requests
almost always have stage-specific error types.

Suggested fix: a `map_err` / `map` / `and_then` adapter (or in-line
form `tokenize(src).map_err(_report_lex_err)?`) on `Result`. Even
just allowing a `with` clause to specify a per-binding mapper would
collapse the 25-line shape back to 6.

## R2-7. Split `let` declarations (declare-then-assign-in-arms) aren't supported (severity: MEDIUM)

```ty
let sp_if: Span                  # ❌ `let` without initializer
match if_kw_res:
    case Err(e): return Err(error=e)
    case Ok(sp_v): sp_if = sp_v
```

The natural Rust idiom — `let x; match f() { Ok(v) => x = v, Err(e) =>
return Err(e) }` — is unavailable. The only ways out are `?`
propagation (only works if errors unify) or full nesting inside the
`Ok` arm. Combined with R2-6, multi-stage heterogeneous pipelines have
no clean shape at all.

## R2-8. Sealed-union variants need wrapper classes when the inner type is also independently used (severity: MEDIUM)

`07-game-ecs` declares `Position` both as a standalone component
(stored in `World.positions: dict[int, Position]`) *and* as a variant
of the `Component` sealed union. The variant must be a distinct
nominal class, so:

```ty
pub class Position frozen: pos: Vec2

# Required wrapper for the sealed-union membership:
pub class CompPosition: data: Position
pub type Component = CompPosition | CompVelocity | CompHealth | ...
```

Seven components → seven wrapper classes whose only field is `data: T`.
Field name "data" is meaningless. A `pub variant CompPosition = Position`
or "any class can be a member of multiple sealed unions" would remove
~30 lines of pure boilerplate per ECS.

## R2-9. Re-emitting a matched variant from an arm forces per-field positional forwarding (severity: HIGH)

The Raft `Node.handle(msg: Message) -> ...` is a 7-arm `match` where
each arm wants to call a typed helper like `_on_append(ae)` where `ae`
has type `MsgAppendEntries`. Today there is no `case X(...) as v:`
binding form, so each arm has to destructure into per-field locals
and forward all of them:

```ty
case MsgAppendEntries(ae_term, ae_leader, ae_pi, ae_pt, ae_ents, ae_lc):
    return self._on_append(ae_term, ae_leader, ae_pi, ae_pt, ae_ents, ae_lc, now)
```

The `_on_append` helper then takes six positional parameters because
passing `msg` typed as `Message` (the union) doesn't narrow on the
callee side, and there's no way to express "the variant value typed as
its specific variant". `node.ty` grew by ~120 lines purely from this.

Suggested fix: support `case Variant(...) as bound:` like Python does,
with `bound` typed as `Variant` (not the union). This is a small
desugar change.

## R2-10. Partial update of a sealed-union-typed field requires factory + full reconstruction (severity: MEDIUM)

```ty
# Inside impl Node, with `role: Role` declared on the class.
# Goal: bump heartbeat deadline without touching next_index/match_index.
self.role.heartbeat_deadline = next_hb        # ❌ Role is a union, no such attr

# Workaround:
match self.role:
    case RoleLeader(ni, mi, _):
        self.role = make_leader(ni, mi, new_deadline)
    case RoleFollower(_, _): pass
    case RoleCandidate(_, _): pass
```

Combined with R2-9, role-state transitions in the Raft simulator are
3× the LoC they should be.

## R2-11. `dict[Newtype, X]` is awkward; teams default to `dict[int, X]` + wrap/unwrap (severity: MEDIUM)

`newtype NodeId = int`, `dict[NodeId, X]` works in principle but every
container access in Raft (`next_index: dict[NodeId, LogIndex]`,
`votes: dict[NodeId, bool]`) ends up coercing through `int()` somewhere.
The path of least resistance in `10-distributed-kv` was to declare
every dict as `dict[int, X]` and wrap/unwrap newtype keys at each
access site — which defeats half the point of having the newtype.

Either dict keys should accept newtypes natively (the emitted Python
is identical) or the docs should call this out and recommend a
workaround pattern.

## R2-12. Newtype arithmetic forces `int(x)` ⟶ `Wrap(y)` round-trips (severity: HIGH)

```ty
# LogIndex and Term are both `newtype X = int`.
let new_idx: LogIndex = self.log.last_index() + 1   # ❌ no `+` between LogIndex and int
mut n: int = int(last_idx)
while n > int(self.commit_index):
    ...
self.commit_index = LogIndex(n)
```

Same-newtype arithmetic and comparison (`+`, `-`, `<`, `<=`, `>`,
`>=`, `==`) is the entire arithmetic surface of Raft's commit-index
advance, term comparison, and log-index math. Forcing
`int(...)` / `Wrap(...)` on every operand drowns the algorithm.

Suggested fix: allow same-newtype arithmetic (`LogIndex + LogIndex`,
`LogIndex < LogIndex`) and one-sided arithmetic with raw `int` literals
(`LogIndex + 1`). Cross-newtype arithmetic should remain forbidden.

## R2-13. `dict[K, list[V]].setdefault(k, []).append(v)` doesn't type-check (severity: MEDIUM)

The textbook "append to bucket or create" pattern fails:

```ty
per_term_positions.setdefault(tok, []).append(pos)   # ❌ checker says missing_attribute
```

Forced to write a 4-line `get → if None → init → append → write-back`
sequence in the inverted-index ingest loop. Inverted indexes,
adjacency lists, and group-bys all live on this pattern.

## R2-14. No first-class `set[T]` / `frozenset[T]` idiom in the example apps (severity: LOW)

`09-search-engine`'s query evaluator needs set operations (intersect,
union, complement) but the only idiomatic shape is
`dict[int, bool]` + `.get(k) is True`. Native `set[T]` would shrink
the engine by ~30%.

## R2-15. `list[T].sort(key=...)` needs a top-level named callback (severity: LOW)

```ty
hits.sort(key=lambda h: h.score, reverse=True)   # untyped lambda — implicit Any
```

Workaround:
```ty
def _hit_sort_key(h: ScoredHit) -> float:
    return h.score
hits.sort(key=_hit_sort_key, reverse=True)
```

Typed lambda syntax (`(h: ScoredHit) -> float => h.score`) or
inference of the parameter type from `list[T].sort` would remove the
trivial top-level helper.

## R2-16. Mixed-source carrier types (newtype + sentinel int) silently degrade to bare `int` (severity: MEDIUM)

`10-distributed-kv`'s outbound message carrier is `list[tuple[int, Message]]`,
where the int is "recipient node id", but a special `-1` sentinel
means "client". Declaring it `list[tuple[NodeId, Message]]` would be
right for 17 of the 18 producer call sites but wrong for the
`-1` sentinel — forcing the declaration back to `int` and forcing
`int(peer)` on every producer. A `Recipient = NodeId | ClientSentinel`
sealed union escapes this but at the cost of a per-delivery dispatch.

## R2-17. `for k in dict_a:` is treated as re-declaration of an outer `let k` (severity: LOW)

```ty
for p in puts:
    let (rid, k, v) = p          # introduces `k`
    ...

for k in leader_node.store.data:  # ❌ re-declares `k`
    ...
```

Python's for-loop variable is just an assignment; Typhon treats it as
`let`-style binding that collides with a same-named outer `let`. The
diagnostic is sensible ("illegal re-assignment") but the fix —
renaming one of them — is tedious in deeply nested code. Round 1 #6
covered `let` inside `case`; this is the for-loop counterpart.

## R2-18. Module-level `freeze let` for `tuple[str, ...]` works and round-trips cleanly (positive note)

`09-search-engine` declares `freeze let STOPWORDS = ("a", "an", ...)`
at module level and it compiles to a real deep-frozen tuple via
`__typhon_freeze__(...)`. Read-only constants are now ergonomic. The
only sad part is R1-#3 — adding `pub` to `freeze let` still parse-errors.

## R2-19. Generic methods on a generic `impl[K, V]` class work (positive note)

`DataLoader[K, V]` with `impl[K, V] DataLoader[K, V]: def load(self, key: K) -> V?:`
compiles cleanly and the emitted Python is the expected PEP 695
`class DataLoader[K, V]:` plus methods. First-class generics-in-impl
works as documented.

## R2-20. The `?` desugar produces beautifully readable Python (positive note)

```py
__typhon_q_0__ = self.expect_kind("ident")
if isinstance(__typhon_q_0__, __typhon_Err__):
    return __typhon_q_0__
key_t: Lexeme = __typhon_q_0__.value
```

Clean temporaries, no exceptions, no callbacks, no monadic chains.
Stack traces from `?`-propagation paths read like ordinary Python.

---

## Round 2 summary by app

| app | initial errors (post-R2-1) | root cause | workaround |
|---|---|---|---|
| 06-graphql-server | 2 | R2-3 (impl on union alias) + unused import | demote `kind()` to `field_kind(f)` free fn; drop the import |
| 07-game-ecs | 1 | unused import | drop import |
| 08-mini-compiler | 6 warnings | R2-2 (nullary frozen variants) | accept warnings; R2-4 stdlib collision forced `types.ty` → `lang_types.ty` and `ast.ty` → `lang_ast.ty` |
| 09-search-engine | 0 | — | clean first try (after R2-1) |
| 10-distributed-kv | 3 | R2-17 (`for k` re-declare) | rename loop variables |

After fixes all five `tyc check` clean (08 keeps warnings), `tyc build`
clean, and `python build/main.py` runs end-to-end.

## Round 2 — biggest blockers, in order

1. **R2-1** — `pub def` invisible to `?` validator (real bug, one-line fix applied).
2. **R2-4** — stdlib name collisions silently break runtime imports.
3. **R2-12** — newtype-int arithmetic forces constant round-trips.
4. **R2-9** — re-emitting a matched variant needs per-field forwarding.
5. **R2-6** — heterogeneous-error `with`-chains have no clean shape.
6. **R2-3** — `impl` on sealed-union alias rejected.

## Round 1 issues that still hit Round 2

| Round 1 # | hits in Round 2 |
|---|---|
| #1 variant→union upcast | every sealed union (Component ×7, Event ×6, ResolvedField ×9, Token ×9, Expr ×13, Ty ×6, Value ×6, Query ×5, Message ×7, Command ×3, Role ×3) — ≈100 factory functions across the 5 apps |
| #3 `pub freeze let` rejected | hit in `09-search-engine` (STOPWORDS) and `10-distributed-kv` (CLIENT_RECIPIENT); workaround was always "drop pub" |
| #5 exhaustive match + missing_return | ~70 `raise RuntimeError("unreachable")` trailers across the 5 apps |
| #6 per-arm `let` shadow | every `case` arm with locals got a variant-specific suffix |
| #9 pattern positional arity | every `case X(...)` padded to the dataclass's full field count |
| #10 unused-import on type-only uses | kept type-only imports for documentation; one was removed only because it was genuinely unused |

The Round 1 findings are now well-understood and bake-in cleanly as a
workaround vocabulary. Round 2's new findings (R2-1 through R2-20) are
about the next layer — what breaks when you push to recursive ASTs,
heterogeneous error pipelines, deep sealed unions, and stdlib-shaped
module names.
