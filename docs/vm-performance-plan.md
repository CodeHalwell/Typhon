# VM performance plan

`docs/vm.md`'s [Performance](vm.md#performance) section states the
numbers plainly: at the start of this plan, `tyc run`'s tree-walking VM
measured **5–18× slower** than `tyc build` + CPython 3.13 at
steady-state compute, in exchange for winning on startup latency and
skipping the build step entirely. This document is the engineering plan
behind that gap: what was measured, why the VM was that slow, and the
tiered plan to close the distance. **Tier 1 has since landed** — see its
measured-outcome table below; Tiers 2–3 remain the forward plan.

---

## Measured baseline

**Methodology.** Release `tyc` binary, CPython 3.13, median of 3 runs per
benchmark, VM and compiled-path outputs diffed and confirmed identical
(parity-checked) so the comparison is apples-to-apples on a *correct*
program rather than a VM bug racing a working build. Fixed per-process
startup cost (interpreter init on the VM side; process spawn + module
import on the compiled side) is measured separately via a hello-world
program and subtracted from both sides before computing the
"startup-adjusted slowdown" column below — that isolates steady-state
compute cost from overhead every invocation pays regardless of workload.

| benchmark | tyc run (VM) | tyc build + python3.13 | startup-adjusted slowdown |
|---|---|---|---|
| fib(27) recursive int | 569 ms | 68 ms | ~18× |
| 3M-iteration while accumulator | 2168 ms | 273 ms | ~9× |
| 300k object constructions + method calls | 733 ms | 189 ms | ~5× |
| dict writes + list comprehension | 387 ms | 85 ms | ~8× |
| hello-world (startup) | 21 ms | 38 ms | VM wins startup |

Benchmark shapes (these are ad hoc scripts used for this measurement
pass, not yet part of the committed `examples/`/`stress/` corpus or the
Criterion suites in `docs/performance-baseline.md` — landing them as a
reproducible fixture set, the way `scripts/perf-gate.sh` did for the
build pipeline, is a reasonable follow-up):

- **`fib(27)` recursive int** — a naive, non-memoized doubly-recursive
  Fibonacci over plain `int`. Dominated by function-call overhead and
  integer arithmetic; no container/allocation work at all.
- **3M-iteration `while` accumulator** — a single `while` loop with no
  function calls, reading and updating a counter/accumulator 3,000,000
  times. Isolates scope-lookup and int-op cost from call overhead.
- **300k object constructions + method calls** — instantiate 300,000
  instances of a small class and call one or more methods on each.
  Stresses `__init__` dispatch, per-call `Env` allocation, and method
  resolution.
- **dict writes + list comprehension** — writes into a `dict` in a loop
  plus a `list` built via comprehension. Allocation- and hashing-heavy,
  comparatively call-light.

The spread (~5× to ~18×) reflects that the four costs below are not
evenly weighted across workloads.

---

## Why it's slow

Four architectural costs, all sitting on the hottest paths (arithmetic,
variable access, calls):

1. **Every `int` is a heap-allocated `BigInt`.** `Value::Int(BigInt)` —
   `tyc/crates/tyc-vm/src/value.rs:360`. Every arithmetic op constructs a
   new one, e.g. `(Int(a), Add, Int(b)) => return Ok(Int(a + b))` —
   `tyc/crates/tyc-vm/src/interp.rs:3197`. `tyc build` inherits CPython's
   own int implementation (small-int cache, machine-word fast path) for
   free; the VM pays a heap allocation on every single `+`.
2. **Every variable read is a string-hash lookup that walks a parent
   scope chain.** `tyc/crates/tyc-vm/src/env.rs:16-74` — `Env::get`
   (lines 66-74) checks this frame's `HashMap<String, Value>`, then
   recurses into `parent` if the name isn't found there. A name captured
   N closures up costs N hash lookups instead of one resolved offset.
3. **Every call allocates a fresh `Env`.** `Env::new_child` wraps a new
   `HashMap` in a new `Rc` — `tyc/crates/tyc-vm/src/interp.rs:2595` — for
   every call, including a two-line leaf function at the bottom of a hot
   recursive path like `fib`.
4. **Method resolution has no cache.** `find_method` walks
   `class.methods`, then recurses into `class.bases` —
   `tyc/crates/tyc-vm/src/interp.rs:2895-2905` — on every method call,
   re-walking the MRO from scratch instead of remembering where `name`
   resolved last time.

---

## Tier 1 — representation & dispatch tuning (landed)

Tier 1 doesn't change the execution model — it's still a tree-walker. It
replaces four hot-path data structures with cheaper ones, targeting the
four costs above:

- **Small-int fast path.** `Value::Int` becomes a canonical `Small(i64)` /
  `Big(Rc<BigInt>)` representation. Arithmetic on two `Small` operands
  uses checked-overflow ops (`checked_add` / `checked_mul` / …) and only
  promotes to `Big` (allocating a `BigInt`) when the true result doesn't
  fit in an `i64`. **Semantics are unchanged** — arbitrary precision is
  fully retained, `2 ** 100` still computes exactly; promotion is an
  invisible representation optimization. Addresses cost 1.
- **Flattened per-class method cache.** A cache on each `Class` mapping
  method name → resolved `Function`, populated lazily on first lookup and
  invalidated on the two mutation points that can change resolution: an
  `impl` block or `extend` block merging new methods into a class. Turns
  `find_method` from an O(depth) walk into an O(1) hit after the first
  call. Addresses cost 4.
- **Direct method-call path.** Skips boxing a `BoundMethod` value for the
  common `obj.method(args)` shape and dispatches straight from receiver +
  cached `Function`, removing an intermediate heap allocation from every
  method call.
- **Slot-resolved locals.** Functions with no `global`/`nonlocal`
  declarations and no captured closure variables — the common case —
  resolve locals to a fixed integer slot computed once at
  function-definition time, instead of a `HashMap` key hashed on every
  read/write. Functions that *do* close over outer scope or touch
  `global`/`nonlocal` keep the existing `HashMap`-backed `Env` as a
  fallback. Addresses cost 2 directly, and reduces cost 3's per-call
  weight (an array of slots is cheaper to allocate and populate than a
  `HashMap`, though a per-call allocation still happens).

**Measured outcome** (same methodology as the baseline table — release
binary, median of 3, parity-checked; compiled-side numbers re-measured
the same day):

| benchmark | VM before | VM after Tier 1 | VM speedup | startup-adjusted slowdown (was) |
|---|---|---|---|---|
| fib(27) recursive int | 569 ms | 400 ms | 1.42× | ~14× (was ~18×) |
| 3M-iteration while accumulator | 2168 ms | 650 ms | **3.3×** | ~3× (was ~9×) |
| 300k object constructions + method calls | 733 ms | 567 ms | 1.29× | ~3.5× (was ~5×) |
| dict writes + list comprehension | 387 ms | 238 ms | 1.63× | ~5× (was ~8×) |

The 5–18× range compressed to roughly **3–14× startup-adjusted (~2.7–6×
end-to-end wall clock)**. Loop-shaped code — where slot-resolved locals
and the small-int path compound — gained the most; recursive call-heavy
code (`fib`) remains the outlier because per-call costs (a real Rust
frame, argument binding, frame setup) dominate it, and those are
precisely Tier 2's target. This was a tuning pass over the existing
tree-walker; Tier 2 is where the larger structural win lives.

---

## Tier 2 — bytecode compilation (designed, not started)

Lazily compile each function body to a compact register bytecode the
first time it's called, and cache the compiled form on the `Function`.
The tree-walking evaluator stays in the loop as the fallback for cold or
rarely-hit AST shapes the bytecode compiler doesn't cover yet, so Tier 2
doesn't need full-language coverage on day one to pay off on the hot
paths that matter (arithmetic, calls, loops).

Sketch:

- **Per-function code objects.** A `CodeObject` per compiled function — a
  flat instruction stream plus a constant pool (literals, referenced
  globals) resolved once at compile time instead of re-walked from the
  AST on every execution.
- **Register frames.** A call allocates one flat array of `Value`
  registers sized from the function's local count — the same slot
  numbering Tier 1 introduces for slot-resolved locals — instead of
  recursing through `eval_expr` / `exec_stmt`. Locals, temporaries, and
  arguments all live in the same register file.
- **Constant pools.** Literals and captured constants are interned once
  per `CodeObject` and referenced by index, instead of re-materialized
  from AST literal nodes on every visit.
- **Jump-threaded dispatch loop.** A tight `loop { match opcode { ... } }`
  over the instruction stream (Rust has no computed-goto, so this means a
  branch-predictor-friendly dispatch loop, not literal threaded code)
  replaces the recursive tree descent.

This is where rough CPython parity on most code becomes realistic — a
register-bytecode loop pays dispatch overhead once per instruction
instead of once per AST node visit, the same lever CPython itself pulled
long ago.

**Validation strategy:** the AST tree-walker remains the semantic
reference. A differential test harness runs both engines over the
`examples/`/`stress/` corpus (plus `tyc build && python3.13` as the third
leg, matching today's VM/CPython parity discipline) and fails on any
output divergence between them.

---

## Tier 3 — type-directed specialization (design sketch)

The VM currently ignores everything `tyc-types` computes — it interprets
the preprocessed Typhon AST directly and (re)derives each value's runtime
type from its `Value` tag at every operation, the same way CPython does.
But unlike CPython, Typhon has already run a full structural type checker
over the program by the time it would execute: `tyc-types` knows,
statically, that a given local is an `int`, that a call's receiver is
exactly `Point`, that a loop variable never changes type. Tier 3 is the
design sketch for feeding that information into the Tier 2 bytecode
compiler.

Where a register's type is statically known and fits a specialized
representation, the compiler could emit specialized opcodes: unboxed
`i64`/`f64` registers for statically-`int`/`float` locals (no `Value`
tag, no `Small`/`Big` branch, no allocation), and direct-dispatch call
opcodes when a call's receiver class is statically known (skip the
method cache and jump straight to the resolved `Function`).

**This is realistic to surpass CPython on statically-typed numeric hot
paths** — a loop over a statically-known `int` accumulator has zero
dynamic-dispatch tax to pay once type-directed opcodes exist, where
CPython (pre-JIT) always pays it. It is **not** realistic across the
board: CPython's built-in `dict`/`str`/`list` implementations are
hand-tuned C with decades of optimization behind them, and CPython
itself isn't a fixed target — the 3.14 tail-call interpreter and its
still-maturing JIT keep moving the baseline this plan compares against.
"Beat CPython everywhere" isn't a credible goal here; "beat CPython on
the paths Typhon's type system can prove are safe to specialize" is.

No implementation timeline. This section exists so Tier 2's register /
slot design doesn't foreclose the option — typed opcodes need a register
file and a real instruction set to mean anything.

---

## Structural caveats

Independent of the tiers above, three properties of the VM's design are
worth naming plainly rather than papering over:

- **No cycle collector.** The heap is `Rc`-based; reference cycles leak
  for the process lifetime. Invisible in short-lived `tyc run`
  invocations and typical REPL usage, but a production-grade
  long-running VM would need a real cycle collector (or a tracing GC) —
  none of the tiers above address this.
- **`Rc` is single-threaded.** `Rc<T>` is not `Send`/`Sync`, so the VM
  cannot exercise Typhon's free-threaded / `gather`-parallel semantics.
  Programs that need real parallelism (`[python] free-threaded = true`,
  the auto-parallel comprehension rewrite) only get it through
  `tyc build`; the VM runs them correctly, just sequentially.
- **Eager generator materialisation.** Already documented in
  [What the VM does not support yet](vm.md#what-the-vm-does-not-support-yet)
  — a `yield`-bearing function runs to completion into a buffer rather
  than suspending a live frame. This is as much a semantic gap as a
  performance one (unbounded generators hit `GENERATOR_CAP`), and none
  of the three tiers above change it; that needs real frame suspension,
  which only a bytecode or continuation-based engine can support
  cleanly.

**Strategic recommendation:** don't chase parity on every axis. The VM's
job is developer-loop latency — `tyc run`, the REPL, and future
LSP-driven expression evaluation, where "runs in tens of milliseconds and
behaves identically to the compiled program" matters more than "runs as
fast as compiled CPython." `tyc build` already owns production
performance, real parallelism, and the full CPython ecosystem; the VM
doesn't need to duplicate that job, only to stay fast enough that
reaching for it during development is never the wrong call.

---

## Non-goals for now

- **PyO3-backed FFI for unsupported modules.** Letting the VM reach into
  real CPython C-extension modules (`numpy`, `pydantic`'s C core, …) via
  PyO3 instead of raising `ImportError` and pointing at `--compile`. Not
  scheduled — it would reintroduce a CPython dependency into what is
  currently a pure-Rust, no-CPython-required execution path, which cuts
  against the startup-latency value proposition (see [Why a
  VM?](vm.md#why-a-vm) in `docs/vm.md`) and needs weighing against that,
  not just against engineering cost. `tyc run --compile` already covers
  this need today.
- **A full JIT.** Tier 3's type-directed opcode specialization captures
  most of the achievable win on statically-typed hot paths at a fraction
  of the cost of a tracing or method JIT (dynamic recompilation,
  deoptimization guards, on-stack replacement). Revisit only if Tier 3
  ships and measurement shows it isn't enough.

---

See [`docs/roadmap.md`](roadmap.md) → **Concrete next steps** for how
this fits the broader delivery plan, and [`docs/vm.md`](vm.md) →
**Performance** for the user-facing summary this plan backs.
