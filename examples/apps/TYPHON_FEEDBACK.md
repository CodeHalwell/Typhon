> ## ⚠️ HISTORICAL DOCUMENT — captured against tyc 0.5.2
>
> **This is not a current bug list. Do not triage from it.** These findings were
> captured against **tyc 0.5.2**; the current version is **v0.15.7**. **Most of
> the friction documented below has since been resolved** — for example
> cross-module variant → union flow, `await` on an `Awaitable` (R3-1),
> multi-line `go` (R3-2), and the exhaustive-match `missing_return` gap are all
> fixed (see `CHANGELOG.md` for the v0.6.x–v0.15.x line). It is retained
> verbatim as a historical record of the dogfooding round that drove much of
> that work, **not** as a description of the language as it stands today. Before
> acting on any item here, confirm it against the current `CHANGELOG.md`,
> `docs/language.md`, and a fresh `tyc check`.

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

---
---

# Round 3 — five more apps (11-realtime-game-server through 15-stream-processor)

A third batch of five large multi-file apps was built to push areas the
first two rounds didn't exercise: an async **multiplayer game server**
with lobby + per-room tick loops, a **static site generator** with full
Markdown + template-inheritance pipelines, an **in-memory vector
database** with HNSW + a filter DSL, an **API gateway** with circuit
breakers + middleware composition + retries, and a **Flink-lite stream
processing engine** with windowing + watermarks. The five apps total
~9,800 lines of Typhon across 72 `.ty` files. Tested against **tyc 0.6.1**.

All five `tyc check src/` clean (0 errors, 0 warnings) and the demo
entrypoints run end-to-end under CPython 3.13. The findings below are
**new** issues that only appeared in Round 3 — Round 1 / Round 2
patterns that bit again are summarised in the table at the end.

## R3-1. `await` on a `Callable[..., Awaitable[T]]` does NOT unwrap to `T` (severity: HIGH — biggest Round-3 finding)

The natural shape of an async middleware chain is

```ty
type NextFn = Callable[[Req], Awaitable[Resp]]

async def caller(nxt: NextFn) -> int:
    let r: int = await nxt(req)   # ❌ tyc::type_mismatch — expected Resp, found Awaitable[Resp]
    return r
```

The checker accepts the alias but rejects the `await`. Same failure
with `Coroutine[object, object, T]`. Every async-middleware design
pattern from FastAPI / Starlette / aiohttp transliterates poorly into
Typhon today.

The `14-api-gateway` workaround was to abandon the `next`-style
recursive chain entirely and rewrite middleware as a concrete class
with **sync** `pre_hook` / `post_hook` `Callable` fields composed as a
straight-line pipeline inside one `async def handle_request(...)`.
This loses the "any layer can wrap an async section around its inner"
power that real middleware frameworks expose — the canonical
"`@asynccontextmanager`-style middleware" can't be expressed.

This is the **single biggest Round-3 finding** and the area where the
language most directly limits idiomatic async Python ports.

## R3-2. Multi-line `go expr(...)` invocations are a hard parse error (severity: MEDIUM — real bug)

```ty
async def f(a: int, b: int) -> None: ...
async def g() -> None:
    go f(
        a,
        b,
    )                 # ❌ tyc::parse — "Simple statements must be separated by newlines or semicolons"
    go f(a, b)        # ✅ same call on a single line
```

`go` appears to be lexed as a special statement head and the inner
call's open-paren is not honoured for line continuation. The error
cursor lands on the *callee identifier*, not the trailing comma, which
makes the cause non-obvious. Found in `11-realtime-game-server/lobby.ty`
when spawning a long-argument-list room tick loop.

This is asymmetric — every other call form in Typhon respects implicit
continuation by parens.

## R3-3. `async with` block does not satisfy `missing_return` analysis (severity: MEDIUM)

The async twin of Round-1 #5:

```ty
async def f() -> int:
    async with lock:
        match thing:
            case Ok(v): return v
            case Err(e): raise HTTPException(400, str(e))
        # putting the unreachable trailer here does NOT satisfy the checker
    raise RuntimeError("unreachable")   # ✅ must live OUTSIDE the with-block
```

Found in `13-vector-db/api.ty`. The trailing `raise` has to be hoisted
out of every `async with` block, even when every reachable arm
terminates. This is *almost* R1-5 but with the additional twist that
the in-block position is rejected.

## R3-4. `from X import Y` inside an `if` / `for` body silently breaks name resolution (severity: MEDIUM)

```ty
if condition:
    from event import make_record_envelope     # parses OK
    let env: Envelope = make_record_envelope(...)   # ❌ tyc::unknown_name
```

The parser accepts the local import (no `parse` diagnostic) but the
resolver doesn't treat the import as binding a name in the surrounding
scope. The error then points at the *call site*, not the import,
making the root cause invisible. Workaround: hoist all imports to
module scope.

Found in `15-stream-processor/runner.ty`. Cost about 30 minutes to
diagnose because the error message is misleading.

## R3-5. Per-arm `let` shadow rule extends to sibling `if` branches (severity: MEDIUM — extension of R1-6)

Round-1 #6 noted the rule for `match` arms; Round-3 confirms it also
fires across adjacent `if` blocks in the same function:

```ty
if is_watermark(env):
    let wm: WatermarkTs = ...           # ✅
    ...
if is_record(env):
    let wm: WatermarkTs = ...           # ❌ tyc::no_block_shadow
    ...
```

Even though only one branch is reachable per call, sibling `if`
branches "see each other's bindings" the same way sibling `case` arms
do. Forced per-branch suffixing (`wm_after_wm`, `wm_after_rec`).

Same underlying fix as R1-6: relax the no-shadow rule for sibling
branches whose control flow proves they're mutually exclusive.

## R3-6. `asyncio.Queue[T]` field annotations require a forward-reference string (severity: LOW)

```ty
class Lobby:
    finalise_queue: asyncio.Queue[FinalisedMatch]            # ❌ rejected — subscripted runtime-only generic
    finalise_queue: "asyncio.Queue[FinalisedMatch]"          # ✅ quoted string works
```

Technically correct Python semantics (`asyncio.Queue` doesn't support
`__class_getitem__` at runtime in older spots), but `dict[K, V]` /
`list[T]` are silently accepted as field annotations even though the
same rule applies. Friendlier behaviour would be to auto-quote the
annotation at desugar.

Found in `11-realtime-game-server/lobby.ty`.

## R3-7. `argparse.Namespace` is the only typeable shape for command-handler `args` (severity: MEDIUM)

```ty
def cmd_build(args: argparse.Namespace) -> int:
    let url: str = str(args.url) if args.url is not None else DEFAULT_URL  # args.url is Any
```

Every CLI command handler that takes `args = parser.parse_args(...)`
must annotate it as `argparse.Namespace`, but the resulting `args.foo`
accesses are typed `Any` (Namespace has no static schema). Each field
read needs an explicit cast, and `model`-style argparse integration
isn't possible today. Found in `12-static-site-gen/cli.ty` — argparse
is the single largest no-static-typing surface in the suite.

## R3-8. `let x: T` declare-without-initialiser is still rejected in `match`/`if` arms (severity: MEDIUM — R2-7 echo)

The "declare-then-assign-in-arms" idiom remains unavailable in Round 3:

```ty
let loaded: tuple[RouteTable, dict[str, Balancer]]    # ❌ no initialiser
match _load(...):
    case Ok(v): loaded = v
    case Err(e): ...
```

`14-api-gateway/state.ty` hit this in `build_state` and had to extract
a `_load_or_default(...)` helper purely to give the binding an
initialiser at declaration. R2-7 covered the same finding from Round 2;
recording the second occurrence as additional evidence.

## R3-9. Ternary `isinstance` narrowing is asymmetric on the `else` branch (severity: LOW)

```ty
let acc_int: int = int(acc) if isinstance(acc, int) else 0   # ✅
let acc_int: int = acc if isinstance(acc, int) else 0        # ❌ acc is still object on the then-branch (inference too weak)
```

The check that narrows `acc` to `int` on the truthy branch of a
ternary doesn't quite carry the same information the equivalent
`if`/`else` block carries. Workaround is a trivial `int(acc)` wrap.
Found in `15-stream-processor/stream_op.ty`.

## R3-10. Parametric sealed unions were avoided pre-emptively (severity: open question)

`15-stream-processor` was the obvious candidate for `EventEnvelope[T] =
RecordEnv[T] | WatermarkEnv | BarrierEnv`. Based on R1-#1
(cross-module variant→union upcast rejected even for *non*-parametric
unions), the agent did not even attempt the parametric form. The
envelope ended up as a concrete `Envelope` class with an `EnvelopeKind`
tag (non-parametric union) and an `object`-typed `record_payload` —
trading payload-type safety for compileability. Real stream / reactive
engines lean hard on parametric sealed unions; the language story for
them needs to be documented (either "supported, here's how" or "not
yet").

## R3-11. `class` field defaults: all-or-none (severity: LOW)

`new_route(..., timeout_seconds: float = 5.0, requires_auth: bool = False)`
works on a free function, but the same defaulting on a `class` field
list is rejected — constructors need every-or-no default, not mixed.
Documenting the rule (or relaxing it) would help; surfaced from
`14-api-gateway/routes.ty`.

## R3-12. Nested `from X import Y` and nested `def` work — undocumented sanctioned patterns (severity: POSITIVE / docs gap)

Two patterns work cleanly in Round 3 but aren't called out in the
guides:

1. **Function-local `from X import Y`** consumed only inside `match` arms
   (used in `12-static-site-gen/md_parse.render_inline_html` to dodge
   the unused-import diagnostic for type-only references).
2. **Nested `def _helper(p: ArgT) -> None:`** inside another function,
   mutating its argument — used by `12-static-site-gen/cli._build_parser`.

Both are useful idioms and the docs should sanction them so users
don't avoid them out of caution.

## R3-13. `class X frozen: pass` (R2-2) now parses cleanly in 0.6.1 — positive resolution

`13-vector-db/metric.ty` and `12-static-site-gen/models.ty` both
declare nullary sealed-union variants as `pub class MCosine frozen: pass`
and the older `placeholder: int = 0` warning is gone. R2-2's prior
workaround is no longer needed. Pattern shape changes accordingly:
`case MCosine():` (zero patterns) instead of `case MCosine(_):`.

## R3-14. `gather:` accepts `asyncio.to_thread(...)` and bound-method coroutines cleanly (positive)

```ty
gather:
    leaderboard = asyncio.to_thread(store.leaderboard, 5)
    recent = asyncio.to_thread(store.recent_matches, 5)
```

Lowers to a clean `async with asyncio.TaskGroup()`. Bound-method
coroutines also bind cleanly to `Callable[..., Awaitable[T]]` fields
(modulo R3-1's await-unwrap bug — for fields that are *called*, not
awaited-with-`await x()`, this works). Recording as a regression test
target so it doesn't quietly break in a future release.

## R3-15. Generic class + `impl[T]` works inside a single module; cross-module method dispatch is brittle (severity: MEDIUM)

`13-vector-db` and `15-stream-processor` both wrote
`class Collection[D]: ...` + `impl[D] Collection[D]: ...` and
`class Operator[I, O]: ...` + `impl[I, O] Operator[I, O]: ...`. **Inside
one module** this works first try and the emitted PEP 695 is clean.
**Across modules** — when one module exports `Stream[T]` and another
calls `stream.map[U](f)` — the `[U]` argument doesn't always flow
through cleanly, and the agents both fell back to `Stream[object]` to
avoid the friction. Recorded as an open spec question rather than a
hard bug: needs either a focused repro suite or a documentation page.

---

## Round 3 summary by app

| app | initial errors | root cause | workaround |
|---|---|---|---|
| 11-realtime-game-server | ~12 | R3-2 (multi-line `go`), R3-6 (Queue[T] forward-ref), R1-#1 factories | one-line `go`, quoted annotation, ~14 factories |
| 12-static-site-gen | ~6 | R2-6 (heterogeneous-error pipeline), R3-7 (argparse Namespace) | StageError envelope + per-stage match towers; explicit args casts |
| 13-vector-db | ~8 | R3-3 (async with + missing_return), R2-12 (newtype arithmetic) | hoist `raise` out of `async with`, `int(x)` wraps everywhere |
| 14-api-gateway | ~14 | **R3-1 (await on Callable[…, Awaitable[T]])**, R1-#2 (interface cross-module) | sync pre/post middleware (no recursive `next`), concrete `Balancer` class |
| 15-stream-processor | ~10 | R3-4 (import-in-block silent fail), R3-5 (sibling-`if` shadow), R2-12 | hoist imports, rename loop/branch locals, bare-int internals + wrap at boundary |

After fixes all five `tyc check src/` clean (0 errors, 0 warnings),
`tyc build` clean, and the demo entrypoints run end-to-end on CPython 3.13.

## Round 3 — biggest blockers, in order

1. **R3-1** — `await` on `Callable[..., Awaitable[T]]` rejected. Single
   biggest finding: blocks idiomatic async middleware composition.
2. **R3-4** — `from X import Y` inside an `if`/`for` block silently
   breaks resolution with a misleading error site.
3. **R3-2** — multi-line `go expr(...)` is a hard parse error.
4. **R3-3** — `async with` + match doesn't satisfy `missing_return` for
   the trailing-raise position.
5. **R3-5** — per-arm `let` shadow extends to sibling `if` branches
   (extension of R1-6).
6. **R3-15** — cross-module generic-class method dispatch (`Stream[T].map[U]`)
   is brittle enough that agents both fell back to `Stream[object]`.
7. **R3-10** — parametric sealed unions: no docs, agents avoided them.

## Round 1 + Round 2 issues that still hit Round 3

| prior finding | hits in Round 3 |
|---|---|
| R1-#1 variant→union upcast | every sealed union — ≈100 factory functions across the 5 apps. The vocabulary is now muscle memory, but it's still ~5% of the LoC. |
| R1-#2 cross-module interface conformance | 14-api-gateway adopted concrete-class-with-`Callable` shape for both `Balancer` and `Middleware`. Discovery: combined with R3-1 the cost is *higher* than R1 suggested because the natural async-middleware shape is rejected twice. |
| R1-#3 `pub freeze let` rejected | each app left its `freeze let` constants module-private; export went via a `pub def get_default_x()` or via the consumer's import path. |
| R1-#5 exhaustive match + missing_return | ~60 `raise RuntimeError("unreachable")` trailers across the 5 apps. Plus R3-3's async twin. |
| R1-#6 per-arm `let` shadow | renamed sibling-arm captures everywhere. R3-5 extends to sibling `if` branches. |
| R1-#9 positional pattern arity | every `case X(...)` padded to the dataclass's full field count. |
| R2-2 nullary frozen variants | **closed in 0.6.1** — `class X frozen: pass` parses cleanly now (R3-13). |
| R2-4 stdlib module name collisions | all five apps proactively avoided `types.ty`, `parser.ty`, `ast.ty`, `tokens.ty`, `string.ty`, `email.ty`, `operator.ty`, etc. 15-stream-processor renamed `operator.ty` → `stream_op.ty` to dodge it. |
| R2-6 heterogeneous-error `with`-chains | 12-static-site-gen hit this hardest; defined a single `StageError` envelope sealed union and converted at each stage call site. ~60 lines of match-tower where a 6-line `with`-chain would have sufficed. |
| R2-7 declare-then-assign-in-arms | recurred (R3-8). |
| R2-9 variant re-emission needs per-field forwarding | 14-api-gateway breaker `gate()` would have wanted `case StateHalfOpen() as s: s.probe_in_flight = True` — had to reconstruct via factory. |
| R2-11 `dict[Newtype, V]` awkward | every app defaulted to `dict[int, V]` or `dict[str, V]` and wrapped at API boundary. |
| R2-12 newtype-int arithmetic | hit hardest in 11-realtime-game-server (ELO math, ~25 wrap/unwrap pairs) and 15-stream-processor (watermark math). |
| R2-13 `dict.setdefault(k, []).append(v)` | dodged proactively — the long form is now reflex. |
| R2-17 `for k in d` re-declare outer `let k` | dodged proactively by naming loop vars uniquely from the start. |

## What worked really well in Round 3

- **`freeze let` policy/config constants** — every app's `config.ty`
  reads like a spec. Deep-freeze semantics + module-level placement is
  the killer pattern.
- **`newtype` IDs caught real bugs** — `11-realtime-game-server` caught
  two `ClientId`/`RouteId` swaps and a `RouteId`/`UpstreamId` swap
  during construction; `13-vector-db` caught a `VectorId`/`CollectionId`
  confusion.
- **`gather:` blocks** — uniformly clean across all five apps,
  including with `asyncio.to_thread(...)` and bound-method coroutines.
- **`go expr() -> task`** for fire-and-forget worker spawns —
  modulo R3-2's multi-line restriction, the strong-ref task registry
  just works.
- **PEP 695 generic classes + `impl[T]` blocks** — first-try clean
  inside a single module across all four apps that needed them.
- **`Result[T, E]` with `?` propagation when errors unify** — the cleanest
  sequenced-error-handling pattern available. R2-6 only bites when error
  types diverge.
- **The R2-2 fix in 0.6.1** — quality-of-life improvement, removes the
  ugliest of the workarounds.
