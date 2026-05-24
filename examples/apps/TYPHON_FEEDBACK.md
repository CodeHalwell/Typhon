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
