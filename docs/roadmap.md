# Roadmap

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.
>
> The active, sequenced path to the first tagged alpha is the
> [Full Alpha (v1.0-alpha) Release Plan](alpha-release-plan.md) — when it and
> the long-term plan disagree about *sequencing*, the alpha plan wins.

Realistic milestones for one person plus AI assistance. The headline target is a useful subset shippable in twelve months.

## Current release

**[v1.0.0-alpha.9](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha.9) — 2026-08-21.**
A maintenance release on top of alpha.8, with no language change. The
warn-level `tyc::contains_secret_literal` keyword table grows from 16 entries
to 55 — seven overlapping proposals consolidated into a single
longest-first-ordered table (`AUTHORIZATION`, `CREDENTIALS`, `WEBHOOK`,
`SIGNING`, `COOKIE`, `DSN`, the `DB_`-prefixed password / pass / pwd / secret
variants plus a secret-only `APP_` / `CLIENT_` / `JWT_`, the
`ACCESS_`/`AUTH_`/`BEARER_`/`CSRF_`/`JWT_` token variants,
`PRIVATE_KEY`/`PUBLIC_KEY`/`SSH_KEY`/`SECRET_KEY`, each with its
squashed-acronym form) — and one more name-word boundary: an uppercase
keyword directly followed by a lowercase letter (`TOKENs`,
`dbPASSWORDstring`). Alongside it: three more allocation reductions on
compiler AST walks (`auto_gather`'s candidate scan, `module_all_names`, and
`tyc build`'s import scan all borrow from the AST instead of allocating), the
mid-August dependency wave — with the `compact_str` 0.10 bump reverted, since
`get-size2` 0.8 pins 0.9.1 and the duplicate broke the vendored Ruff build —
and docs-site keyboard-accessibility (`<abbr tabindex="0">`) and
reduced-motion (`transition: none` beside `animation: none`) polish. On the CI
side, the T0.2 differential gate's harness now runs at `--jobs 2` (at `--jobs 4`
it finished with the runner's swap 99.7% exhausted), though that does not stop
the gate's intermittent `runner has received a shutdown signal` kills, which
remain unexplained and clear on a re-run. No new syntax and no new
error-level diagnostic.

**[v1.0.0-alpha.8](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha.8) — 2026-08-08.**
A maintenance release on top of alpha.7. One VM ↔ CPython parity fix: the VM's
MT19937 was seeded from a fixed constant when a program never called
`random.seed()`, so an unseeded program was deterministic under `tyc run` and
non-deterministic under `tyc build && python` — the default now seeds from
entropy as CPython does, while explicit `random.seed(n)` keeps producing
byte-identical sequences on both surfaces. (The same bug intermittently flaked
the T0.2 differential gate, because a VM that always agrees with itself slips
past the harness's self-nondeterminism filter.) Alongside it: the warn-level
`tyc::contains_secret_literal` name lint now treats digits and UPPERCASE→TitleCase
junctions as word boundaries (`myPASSWORD123`, `dbPASSWORDString`), two
allocation reductions on AST walks in `parallel_lints` and the desugarer, the
early-August dependency wave (five crates, two GitHub Actions majors), and
docs-site card/link-card transition polish. No new syntax and no new error-level
diagnostic.

**[v1.0.0-alpha.7](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha.7) — 2026-07-29.**
The full-codebase-review remediation release. It closes all ten of the
[2026-07-28 codebase review](codebase-review-2026-07-28.md)'s 1.0 blockers
plus the Tier-0 gates that make them verifiable: type-checker soundness
fixes (instance-attribute assignment is now type-checked, constructor field
order follows reverse MRO, model constructors are keyword-only, recursive
type aliases terminate, parametric sealed-union match exhaustiveness is
checked, and a new nullable-field-dereference check lands at warn level),
resolver fixes (`let` immutability is now enforced inside loop bodies and
through `global`/`nonlocal`), a shared-lexical-mask rewrite of the
preprocessor fixing several literal-corrupting bugs (`gather:` dependency
scanning, `with`-chain parsing, `enum` body rewriting), five
emitter/preprocessor miscompilation fixes, and VM `ExceptionGroup`/`except*`
support (PEP 654) plus five other VM ↔ CPython divergence fixes. Two new CI
gates ship alongside: a VM ↔ CPython differential gate over the full
1342-file example+stress corpus (first run found 126 divergences, now
pinned as a baseline), and an opt-in-knob codegen matrix. No new syntax;
most changes are narrowings on code that was already crashing or relying on
unsound typing, plus one warn-level diagnostic-surface addition.

**[v1.0.0-alpha.6](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha.6) — 2026-07-21.**
A maintenance release on top of alpha.5, driven by the
[2026-07-20 release-readiness review](release-readiness-2026-07-20.md). The
July dependency wave is carried safely across the `toml` 0.8 → 1.x major —
including the fix for the one regression that bump introduced (the
`tyc-venv` dependency allow-list reader silently returning empty, which
would have no-opped third-party arg/type checking) — six squashed-acronym
keywords join the now-shared `tyc::contains_secret_literal` table
(`API_TOKEN`, `APITOKEN`, `API_SECRET`, `APISECRET`, `API_PASSWORD`,
`APIPASSWORD`; warn-level only), the release pipeline's artifact download is
re-pinned to the proven v4 line to match the v4 upload, and a round of
docs-site accessibility, diagnostics-index, and repo-hygiene fixes lands
(including the `.Jules`/`.jules` case-collision that broke fresh checkouts
on macOS/Windows). No new syntax; no previously-*correct* program changes
behaviour.

**[v1.0.0-alpha.5](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha.5) — 2026-07-09.**
A performance-focused release on top of alpha.4. VM performance Tier 1
(`tyc run`) — a two-representation `VmInt`, a per-class resolved-method
cache with a direct dispatch path, and slot-resolved locals — compresses the
VM's steady-state slowdown vs `tyc build` + CPython from ~5–18× to ~3–14×
(startup-adjusted), with CPython's exact arbitrary-precision integer
semantics preserved. A new `[optimise]` config profile + `tyc build -O` give
a single dial over `auto-memoise` / `auto-gather` / `auto-parallel` /
`pgo-memoise`. Seven new advice-only lints (the `tyc::perf_*` family +
`lazy_import_opportunity`) flag hot-loop anti-patterns. A free-threading
parallelisation wave widens `auto-parallel`, adds an integer
accumulator-loop reduction (`auto-parallel-reductions`), two new advice
lints, and a PEP 734 interpreters backend. Native PEP 810 lazy imports land
on `[python] target = "3.15"` targets. No new syntax; every rewrite here is
opt-in or advice-only, so no previously-*correct* program changes behaviour.

**[v1.0.0-alpha.4](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha.4) — 2026-07-08.**
A focused hardening release on top of alpha.3. It closes the one HIGH finding
the release-readiness review deferred — **H5**, scope-blind class unification: a
locally declared class no longer unifies with a same-named foreign class of a
provably different shape (evidence-gated, degrading to the previous permissive
behaviour on any uncertainty, so the example/stress corpus is byte-identically
unchanged). The secret-name diagnostics match longest-first again (`APIKEY`
before `KEY`), the release-engineering hygiene from the alpha.3 review is
finished (SHA-pinned GitHub Actions, a pre-release-aware installer), and a round
of dependency / advisory bumps lands (`crossbeam-epoch` RUSTSEC-2026-0204,
`regex`, `memchr`, `compact_str`). No new syntax; like the alpha.2 diagnostics,
the H5 fix is a conservative narrowing that only rejects programs passing a
provably-different-shaped class across a module boundary, so no
previously-*correct* program changes behaviour.

**[v1.0.0-alpha.3](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha.3) — 2026-07-03.**
A release-readiness remediation pass (`RELEASE_READINESS_REVIEW.md`) — the
licensing, packaging, and robustness counterpart to alpha.2's soundness sweep.
Licensing / packaging gaps that would block a clean public release are closed
(a repository-root MIT `LICENSE`, the upstream Ruff MIT notice vendored beside
the fork, `SECURITY.md` / `CONTRIBUTING.md` / Dependabot, and release-workflow
hygiene — pre-release tags flagged as such, auto-tag gated on green CI). The
compiler and VM gain diagnostic-reporting and complexity fixes (identical errors
at distinct source locations are all reported; nested-generic assignability is
linear again instead of O(2^depth); four more flow-narrowing invalidation holes
closed) plus six VM ↔ CPython parity fixes (cyclic-value comparison, `str.find`
character offsets, in-place list `+=`, float `%` / `//` sign & zero-division,
broken-pipe `print`, `json.dumps` key coercion). Tooling hardens the LSP
(256 MiB stack) and formatter (atomic writes), adds a `TYC_NO_INTROSPECT`
kill-switch and Windows venv discovery, and wires up the
`[strictness] exhaustive-match` knob. No new syntax; the flow-narrowing fixes
are conservative widenings that only affect programs relying on a
previously-*unsound* narrowing, so no previously-*correct* program changes
behaviour.

**[v1.0.0-alpha.2](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha.2) — 2026-06-29.**
The remediation of the [2026-06-28 adversarial pre-release review](adversarial-review-2026-06-28.md):
a type-checker soundness sweep (non-local flow-narrowing invalidated across an
intervening call or alias write; short-circuit `and`/`or` narrowing no longer
false-positives on `x is not None and x.method()`), a batch of newly-typed
positions (slice reads, subscript assignments, tuple-unpack and `match` captures,
walrus bindings, parameter defaults), VM↔CPython parity fixes (`bytes %`,
numeric dict-key collapse, VM `as!` enforcement), three conservative diagnostics
(`tyc::not_a_context_manager`, `tyc::raise_non_exception`,
`tyc::frozen_inheritance_conflict`), and config / crash robustness. No new syntax
and no previously-*correct* program changes behaviour, but — unlike the
purely-additive releases before it — those three diagnostics narrow the accepted
surface for programs that already crashed at runtime, which now surface as
build-time errors instead.

**[v1.0.0-alpha](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha) — 2026-06-22.**
Typhon's first tagged alpha and first *feature-complete* milestone — the proven
production surface plus the previously-deferred type-system frontier. Rolls up
M1 + M2 of the [alpha release plan](alpha-release-plan.md), the early M3 polish
(formatter idempotence, perf-regression CI gate), and the `rescue` boundary
sugar. Frontier work lands: HKT unification (`tyc::kind_mismatch` on bad arity /
conflicting binding), user-generic variance inference (covariant / contravariant
from usage, cross-module, with `@covariant` / `@contravariant` overrides),
variance through generic interface bounds, the inter-procedural field-init
audit, and 2-member non-nullable union modelling. The production path
(`tyc build` → CPython 3.13+) is stable; the full corpus builds to runnable
Python and checks clean. Alpha caveat: surface syntax is not yet frozen and may
change before `1.0.0` with a migration note. Deferred to beta: embedded `ty`
Phase 2 (subprocess Phase 1 ships), typeshed pure-extension checking, and the
function-level HKT tail. Additive on the accepted surface.

**[v0.15.7](https://github.com/CodeHalwell/Typhon/releases/tag/v0.15.7) — 2026-06-21.**
A robustness release deepening compile-time checking of third-party code and
clearing a batch of type-checker false positives surfaced by the 2026-06-21
stress round (~130-program sweep) and a 43-library introspection audit. The
headline: **third-party *method* calls are now arity-checked** — a missing
required argument to `PCA(...).fit()` or `df.merge()` is caught at `tyc check` /
`tyc build` time, the same way constructor and free-function calls already were.
Three introspection-robustness fixes (a proxy member that raises from
`inspect.signature` no longer disables a whole module's checks; the
implicit-Optional `x: T = None` idiom no longer false-positives; `pkg.sub.Thing()`
multi-segment attribute calls are checked like the `from`-import form) plus three
pure type-checker false-positive fixes (`Counter +/- Counter`, a `plain class` /
`class!` with a hand-written `__init__`, and others). Additive on the accepted
surface. (The `rescue` exception-boundary sugar — postfix + block forms — is
landed on the Unreleased line; see [the design note](design/error-boundary-sugar.md)
and the language reference.)

**[v0.15.6](https://github.com/CodeHalwell/Typhon/releases/tag/v0.15.6) — 2026-06-16.**
An ~198-program adversarial sweep ("if you can write it in Python, you can use
Typhon"). Several type-checker false positives (several build-blockers) and a
cluster of VM parity gaps were fixed; compiled output (`tyc build` → CPython
3.13) is now correct on the entire corpus. The largest cluster was custom
exception classes (`class FooError(Exception):` + `raise FooError("msg")`). Two
additive features also landed: flow-sensitive attribute narrowing
(`if self.x is None: return …`) and a VM `abc` module shim. Backward-compatible.

**[v0.15.5](https://github.com/CodeHalwell/Typhon/releases/tag/v0.15.5) — 2026-06-15.**
A bugfix release making `extend BUILTIN:` work across module boundaries.
Previously builtin extension methods (`extend str: def slug(...)`) only rewrote
call sites inside the declaring module; importing the module dropped the
extension, firing `tyc::attribute_not_found` on `title.slug()` in a consumer.
Fixed end-to-end across the type checker, build/codegen, and VM.

**[v0.15.4](https://github.com/CodeHalwell/Typhon/releases/tag/v0.15.4) — 2026-06-15.**
A bugfix release from a layered FastAPI-style app field report. Closes the
cross-module structural-conformance gap the reviewer called "the single biggest
gap", fixes a `pub` modifier-stacking parse error (`pub comptime let`), and
documents the module-local scope of `extend BUILTIN:`. Additive — output is
byte-for-byte unchanged.

**[v0.15.3](https://github.com/CodeHalwell/Typhon/releases/tag/v0.15.3) — 2026-06-15.**
A tooling release with no language, type-checker, VM, or runtime change. Ships
the `typhon` Claude skill *inside the compiler* and adds `tyc install skill` to
vendor it into any project, bringing the bundled skill current with the
v0.14.1 → v0.15.2 surface.

**[v0.15.2](https://github.com/CodeHalwell/Typhon/releases/tag/v0.15.2) — 2026-06-14.**
A bugfix release with no surface change: a quote inside a comment no longer
breaks a following `as!` / `?` (the preprocess left-operand scan is now
comment-aware).

**[v0.15.1](https://github.com/CodeHalwell/Typhon/releases/tag/v0.15.1) — 2026-06-14.**
A performance and polish release with no language or API change. Source-map
generation now runs in O(N log N) instead of O(N²) (`build_source_map_v2` no
longer re-scans from the start for every token offset — a 10k-line file dropped
from ~31 s), plus docs-site accessibility fixes.

**[v0.15.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.15.0) — 2026-06-13.**
A feature release sharpening Typhon at the library boundary, driven by an async-app
field report. Four threads land together: the `as!` checked cast now composes in
any expression position; a `try_result(thunk[, on_err])` combinator collapses the
exception→`Result` boilerplate; the compiler ships curated `.dty` stubs for the
most-imported third-party libraries; and `async_without_await` stops firing on
methods that are async only to honour a contract. A cross-module class-identity
fix and a round of review hardening round it out.

**[v0.14.3](https://github.com/CodeHalwell/Typhon/releases/tag/v0.14.3) — 2026-06-12.**
LSP follow-ups to the 0.14.2 editor work: `typhon.toml` edits now refresh editor
diagnostics live, plus committed end-to-end LSP coverage.

**[v0.14.2](https://github.com/CodeHalwell/Typhon/releases/tag/v0.14.2) — 2026-06-12.**
Async concurrency. `auto-gather` was opt-in, required a `@gatherable` decorator
on every callee, and only considered same-module `async def`s — so the most
common missed-concurrency shape (two awaited method calls on an imported client)
was never surfaced. This release closes both gaps: a default-on
`tyc::gather_opportunity` advice points out independent sequential `await` runs
for *any* awaited callee, and the `auto-gather` rewrite now folds in callees
imported from other project modules.

**[v0.14.1](https://github.com/CodeHalwell/Typhon/releases/tag/v0.14.1) — 2026-06-12.**
Cross-module shape-propagation completeness. A self-hosted API budget tracker
built on v0.14.0 surfaced that four per-module checker tables — `newtypes`,
transparent `type_aliases`, `enums`, and `frozen_classes` — were never threaded
through the cross-module shape registry, so that type information silently went
missing once a declaration was consumed from another module. All four are now
carried across the boundary.

**[v0.14.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.14.0) — 2026-06-12.**
Two ergonomics features from a playground retrospective: the **`as!` checked
boundary cast** (`EXPR as! TYPE`) is the one-line, *sound* replacement for the
`unsafe:`-block-plus-re-assertion dance at every untyped boundary, and runtime
tracebacks now auto-remap to `.ty` source rather than the emitted `.py`. Both
additive — every previously-accepted program type-checks identically.

**[v0.13.2](https://github.com/CodeHalwell/Typhon/releases/tag/v0.13.2) — 2026-06-12.**
A playground stress round: method-call `match`, multi-line `?`, the `gather:`-in-`match`
import, and `tyc fmt` round-trips. Additive on the accepted surface.

**[v0.13.1](https://github.com/CodeHalwell/Typhon/releases/tag/v0.13.1) — 2026-06-11.**
A six-fix patch on v0.13.0 from a round of app-building (plus two
PR-review hardenings): `?` on a bare `async` call now errors with
`tyc::missing_await` rather than silently miscompiling; `await` unwraps a
stored `asyncio.Task[T]` / `Future[T]` (skipping same-named user classes);
`tyc run` resolves the whole project `src` tree before launching the VM;
the VM binds imported `type` sealed-union aliases (forward-declared ones
included, matching CPython's lazy `TypeAliasType`); `tyc fmt` is
string-aware around a `#` inside a `freeze let` value; and `pub enum`
parses. Additive on the accepted surface.

**[v0.13.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.13.0) — 2026-06-11.**
Stress-round fixes (cross-module `extend`, TypedDict-style dict-literal
lowering, enum-match exhaustiveness, the extended Result API) plus a
post-release code review of everything since v0.12.0. The review fixed ten
issues, headlined by two CPython-divergences in the VM — seeded
`random.sample` (the selection-set threshold was computed with the wrong
log base) and `@staticmethod` / `@classmethod` reached through an instance
(the receiver was wrongly bound) — and a type-checker false positive where
`incompatible_override` flagged a valid LSP-widening override that merely
added an optional parameter. Additive on the accepted surface.

**[v0.12.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.12.0) — 2026-06-07.**
VM comparison-protocol parity + deep compile-time library introspection.
`sorted()` / `min()` / `max()` now honour a user `__lt__` (a silent-wrong-output
fix), and `dict.popitem` / `dict.fromkeys` / `str.maketrans` / `str.translate`
landed in the VM. The headline is third-party argument-*type* checking: venv
signature introspection captures parameter and return annotations (scalars,
`Optional` / `X | None`, concrete containers, fixed-arity tuples) so a
wrong-typed call to a fully-typed dependency — **function or constructor** — is
caught by `tyc check` / `tyc build`, **and live in the editor via `tyc lsp`**.
A declared dependency that can't be introspected now surfaces a warning
(`[strictness] unintrospectable-dependency`) instead of silently passing. Phase 1
of the typeshed-backed [`ty` integration](ty-integration.md) shipped — `[checker]
external = "ty"` / `--with-ty` runs Astral's `ty` over the emitted Python with
diagnostics re-attributed to `.ty` source. Additive on the accepted surface.

**[v0.11.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.11.0) — 2026-06-04.**
VM parity sweep + `enum` keyword. A fresh adversarial stress round
against v0.10.0 surfaced 22 findings — almost entirely in the VM, with
a handful of type-checker coherence gaps. This release closes every
finding from the round, lands the proposed `enum` keyword as a
first-class declaration form (`enum Shape: CIRCLE / SQUARE`, sugaring
over `enum.Enum` with `enum.auto()` for bare members), and adds two
new VM value kinds: `Value::Complex` for native complex arithmetic
(with promotion across int / float and hashable for set / dict keys)
and a dict-view kind backing `dict.keys()` / `.values()` / `.items()`
so they repr and behave like CPython. Bare `super()` is now rewritten
to the explicit two-arg form so `@dataclass(slots=True)` no longer
crashes emitted code, `__call__` / `__post_init__` dispatch on
instances, multi-level inheritance accumulates fields across the full
MRO, and the long tail of stdlib parity lands: native `enum` /
`datetime` (naïve / UTC) / `pathlib` / `collections.defaultdict`
shims, real `re.Match` capture groups, banker's `round`, `bytes`
methods, `itertools.groupby(key=)`, `str.split(maxsplit=)` as a
pure-keyword arg, f-string `{x=}` debug conversion, and `str %`
runtime formatting. VM value semantics now match CPython: dataclass
instance eq / repr / hash is value-based, set equality is
order-independent, and float repr matches CPython's shortest
round-tripping form. Type-checker tightening: `None` flows into
`object`, `str %` is type-checked, and `(5).items()` / `5["a"]` /
`for x in 5:` fire at check time. `tyc init` seeds
`allow-secret-comptime = false` in the generated `typhon.toml`. **22
of 22** stress-round findings closed; additive on the accepted
surface except where the VM was returning wrong values
(dataclass-instance equality, set equality, float repr) — those are
now correct.

**[v0.10.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.10.0) — 2026-06-01.**
VM completeness release. Stress-testing the tree-walking VM past v0.9.2
surfaced a batch of correctness and coverage gaps that stopped `tyc run`
from being a drop-in for `tyc build && python` on real-world programs.
The VM now dispatches dunders and rich comparisons (`__add__` + reflected
forms, `__eq__` / `__lt__` / …, `__str__` / `__repr__` / `__len__` /
`__getitem__` / `__contains__`) on user instances, runs finite generators
eagerly (`yield` / `yield from`, capped at 1M items), models `type(x)` as
a real type object (`type(x).__name__`, `type(x) == int`), invokes
`@property` getters on attribute read, binds `cls` for `@classmethod`, and
ships the long tail of missing builtins — `divmod`, `pow` (2- and 3-arg),
`format`, `ascii`, `int(str, base)` (incl. `base=0`), full set algebra,
the missing string methods, `json.dumps(indent=…)`, `time.perf_counter` /
`process_time`, `math.gcd` / `lcm` / `factorial` / `isqrt` / `comb` /
`perm`. `max` / `min` / `list.sort` accept `key=` / `reverse=` /
`default=` kwargs, and pydantic `model_validate` / `model_dump` /
`model_dump_json` make flat `model` classes usable under `tyc run`. The
type checker plugs three exhaustiveness / augmented-assign false positives
(`match` over `bool` / string-literal unions / fixed-arity tuples, and
`s += 5` type-mismatch on scalar targets), and `tyc-emit` shaves heap
allocations out of the literal-emission hot path. Additive on the accepted
surface.

**[v0.9.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.9.0) — 2026-05-27.**
Stress-test cleanup release. Closes **32 findings** from a v0.8.1
stress sweep across the type checker, VM, parser, lowering passes,
diagnostics, and CLI. The VM is now usable as the daily-driver
runner the docs always advertised: `Result` combinators, `open()`
write/append/binary modes, class patterns on built-ins, `frozenset`
as a dict key, deep `freeze let`, comptime inlining, `lazy import`,
`class!` exception fields, dataclass mutable-default factories,
`collections.deque` / `heapq` / `contextlib` / `pydantic` shims,
multi-file projects, and `@property` / `super()` / `@contextmanager`
all work under `tyc run`. The type checker plugs silent-correctness
gaps in Sequence covariance, variant-to-parametric-union flow,
`while True:` reachability, post-loop narrowing, `assert`
narrowing, `*args` annotation policy, `extend list[T]` dispatch,
exhaustive match on `T?`, `with`-chain error mismatch, and the
`comptime let T: type` alias. Additive on the accepted surface.

[v0.8.1](https://github.com/CodeHalwell/Typhon/releases/tag/v0.8.1)
is a point release on top of v0.8.0 that fixes a regression where the
widened `tyc::attribute_not_found` rule false-positived on
venv-introspected third-party Python classes
(`uvicorn.Server.serve(...)`, `httpx.AsyncClient.aclose(...)`,
`fastapi.Request.body(...)`). `InterfaceShape` now carries a
`partial` flag set on every venv-derived shape;
`class_hierarchy_fully_known` returns `false` whenever any class in
the chain is partial. Strictly a bugfix; no language, runtime, or
stdlib changes beyond the carve-out.

Headline changes vs. v0.8.1:

**VM coverage (closing the gap between `tyc run` and `tyc build && python build/main.py`):**

- `Result` combinators (`.map` / `.map_err` / `.and_then` /
  `.or_else`) work on `Ok` / `Err` values via bound `NativeFn`
  wrappers.
- `open()` honours `"w"` / `"a"` / `"wb"` / `"r+"` modes plus
  `__enter__` / `__exit__`. `json.load` / `json.dump` ride on top.
- Class patterns on built-in types (`case str() as s:`,
  `case int() as n:`, …) match; exhaustiveness recognises
  `case None:` + `case str() as s:` as covering `str?`.
- `frozenset(...)` is hashable as a dict key (`HashKey::FrozenSet`).
- f-string `_` thousands separator works the same way `,` does.
- `bytes` repr matches CPython byte-for-byte.
- Native shims for `collections.deque`, `heapq`,
  `contextlib.contextmanager`, `pydantic.BaseModel`.
- `@property` / `@classmethod` / `@staticmethod` / `super()`
  builtins so decorated methods don't crash on import.
- `lazy import np = numpy` uses the simpler `import M as N` rewrite.
- Multi-file projects: sibling `.ty` modules load via the project
  source root, relative imports work, `tyc run --compile` spawns
  `python -m <pkg>.main` for the package-style entry point.
- `dataclasses.field(default_factory=list)` invokes per instance.
- `class!` synthesised `__init__` runs; `except X as e:` binds the
  user `Instance`; exception-type matching walks the MRO.
- `freeze let CFG = {...}` actually freezes (list → tuple, dict →
  mappingproxy, recursive).
- `comptime let X = ...` inlines via the substitution pass shared
  with `tyc build`.
- Typed tuple unpack `let (a: int, b: str) = pair()` parses in the
  VM.

**Type checker (silent-correctness gaps):**

- Sequence / Iterable / Iterator / Collection / Container /
  Reversible covariance for built-in containers
  (`list[Dog] → Sequence[Animal]`). Mapping / MutableMapping cover
  `dict[K, V]` (K invariant, V covariant).
- Variant → parametric sealed union assignability
  (`Cons[T] → LinkedList[T]`).
- `while True:` reachability + post-while-loop narrowing.
- `assert x is not None` narrows.
- `*args` / `**kwargs` require annotations
  (`*args: object` / `**kwargs: object`).
- `extend list:` dispatches on `list[T]`-annotated receivers.
- Exhaustive `match` on `T?` recognises built-in class patterns.
- `with`-chain explicit `else err: return Err(err)` validates the
  error type against the function's declared return.
- `func[T](args)` explicit type instantiation fires a clear
  check-time error.
- `comptime let T: type = int` lowers to a PEP 695 `type T = int`
  alias.
- New `tyc::freeze_not_freezable` diagnostic validates
  `freeze let X = <expr>` at check time.
- `pub *` name collisions surface in `tyc check` (not just
  `tyc build`).

**Diagnostics polish:**

- `interface_not_conforming` arity message rephrased to
  "got N non-self parameter(s), expected M".
- `invalid_question_op` help text covers both the `Result`-return
  cause and the comprehension carve-out.
- Sealed-union impl distribution dedupes diagnostics by
  `(code, rendered message)` so a 10-variant union doesn't report
  10 identical errors.
- `class_attr_shadows_slot` no longer false-positives on
  mutable-literal default fields.
- `MissingAnnotation` text drops double-backtick wrapping.

**Docs:**

- Cheat sheet documents `class X frozen(Base):` ordering and the
  `*args: object` idiom.

**Known limitations carried forward:**

- Preprocess line-number leakage (B15) — diagnostics still report
  preprocessed-buffer line numbers for `impl Alias:` distribution
  over sealed unions. The dedupe pass cuts the *count* of noise
  diagnostics but each surviving diagnostic still points at a
  synthetic line index past EOF of the original source.

---

The earlier feature surface comes from **v0.8.0** (the stress-test
sweep release on top of v0.7.0 / v0.7.1, closing **41 findings**
from a multi-file v0.7.1 stress report spanning the type checker,
VM, parser, lowering passes, diagnostics, and CLI). Phases 0–3 +
Phase 5 / 5.5 / 6 are complete; v0.8.0 lands several long-missing
diagnostic firing sites, a meaningfully larger native VM stdlib,
and five parser scaffolds the docs already advertised.

Headline changes vs. v0.7.1:

- `tyc::attribute_not_found` now fires on class instances and generic
  classes, not just `TypeVar`-bounded parameters. Foreign /
  venv-introspected classes are tracked with a `partial` shape flag
  and keep the permissive degrade-to-`Unknown` behaviour.
- Interface parameter type conformance — `interface_missing_members`
  compares params position-by-position (contravariant) in addition to
  arity.
- `Type::LitStr(String)` — string-literal singleton types via
  `type Color = "red" | "green" | "blue"` and `Literal["a", "b"]`.
- VM upgrades: arbitrary-precision integers (`num_bigint::BigInt`),
  insertion-ordered dicts (`indexmap::IndexMap`), full f-string format
  flags, mapping match patterns, sequence-with-star patterns,
  recursion limit raised to 1000 to match CPython.
- Larger native VM stdlib: `re`, `typing`, `collections`
  (`OrderedDict`, `defaultdict`, `Counter`, `namedtuple`), `functools`
  (`lru_cache`, `cache`, `cached_property`, `reduce`, `partial`),
  `itertools` (`chain`, `count`, `cycle`, `accumulate`, `combinations`,
  `permutations`, `product`, `islice`, `takewhile`, `dropwhile`,
  `groupby`), `dataclasses`, `pathlib`.
- Parser scaffolds: HKT `class Functor[F[_]]:`, `impl[T]
  SealedUnionAlias[T]:` distributing across every variant,
  `class X[T] frozen:`, `async def` in `interface` bodies
  auto-completing the body, outer-annotation tuple unpack.
- Diagnostics polish: synthetic preprocess lines no longer leak into
  source listings; new lint warnings
  (`tyc::empty_collection_no_annotation`,
  `tyc::typing_alias_in_annotation`,
  `tyc::contains_secret_literal`); `tyc::pattern_shadows_outer`.
- CLI polish: `tyc check lib.dty` accepts a single `.dty` file
  directly; `tyc run --compile` rejects single-file inputs up-front;
  `tyc migrate` strips trivial `__init__` methods.
- Default change: `unused_import` is now `warn` (was `error`); restore
  via `[strictness] unused-import = "error"`.

For the per-release breakdown of the v0.3.x / v0.4.x / v0.5.x /
v0.6.x / v0.7.x line — and the canonical phase-by-phase status below
— see [CHANGELOG.md](../CHANGELOG.md) and the [Project
Status](https://codehalwell.github.io/Typhon/introduction/status/)
docs-site page.

Open frontier work (full HKT unification, general inter-procedural
field-init audit) is unchanged from v0.5.0 — see
[`TYPE_SYSTEM_FRONTIER.md`](../TYPE_SYSTEM_FRONTIER.md).

## Phase 0 — Foundation (months 1–2) ✅ complete

- ✅ Fork `ruff_python_parser` and `ruff_python_ast` into `vendor/`. The
  vendored crates are active members of the Cargo workspace; all consumer
  crates use `ruff_python_ast` via `tyc_syntax::parse_module`. The migration
  off `rustpython-parser` is complete — see `tyc/vendor/README.md` for
  migration details.
- ✅ Add one or two custom tokens (`let`, `mut`) to confirm the fork-extend workflow.
- ✅ Round-trip Python through the fork: `tyc-emit`'s hand-written printer
  covers the Python subset used in every built and tested Typhon module;
  round-trip correctness is verified by the integration test suite
  (`cargo test --workspace`). A corpus sweep over third-party Python files is
  a future hardening task, not a blocker.
- ✅ `clap`-based `tyc` shell with `tyc fmt` working as the simplest end-to-end command.
- ✅ `miette` + `thiserror` diagnostic infrastructure.

## Phase 1 — Core types (months 3–5) ✅ complete

- Salsa db with cached `preprocessed_text` and `module_decl_names` queries; richer queries unlock as their outputs become `salsa::Update`-friendly.
- Name resolution and scope construction with module / function / class / comprehension scopes.
- `let` / `mut` enforcement: reassigning a `let` is a hard error; top-level bindings default to `let`.
- Nominal types: function signatures, assignment compatibility, primitive types, classes, generic containers.
- Non-nullable by default with flow narrowing on `is None`, `is not None`, and `isinstance(x, T)` checks. `T?` is sugar for `T | None`.
- `tyc check` emits useful "unknown name", "type mismatch", "nullable use", and "wrong argument count" diagnostics via `miette`.

## Phase 2 — Class and value features (months 6–8) ✅ complete

- ✅ Class emission as `@dataclass(slots=True)`; the `model` keyword for Pydantic, with `extra='forbid'` injected by default (override via `[emit] model-extra`).
- ✅ Sealed unions and exhaustive `match`. (High-value and mechanically simple — front-loaded.)
- ✅ `Result[T, E]` type, the `?` operator, and `with`-chains (multi-line `with name = expr?, …:` sequencing with an optional `else err:` block).
- ✅ Comptime constants with `env()` lookup. Build fails on missing required env.
- ✅ `tower-lsp-server` backend: `tyc lsp` runs on stdio, publishes diagnostics via the check pipeline on `did_open` / `did_change`, and serves a placeholder hover response. The richer hover (symbol type, doc string, definition link) lands once the resolver exposes a `(file, position)` query.

## Phase 3 — Structural typing and advanced features ✅ complete (subset)

- ✅ **Generics syntax decision locked** (PEP 695). The parser accepts
  `def f[T](x: T)` and `type Vector[T] = ...` directly via the vendored Ruff
  parser; the resolver declares type params into the function/class scope and
  the emitter round-trips the `[T]` syntax. Bidirectional inference binds
  typevars from actual arguments (recursively, with conflict-widening) and
  substitutes them in the return type; bounded-type-var checking is in place.
  Full variance and higher-kinded forms are deferred.
- ✅ **Interface declarations** (`interface Name:` → `class Name(Protocol):`)
  with structural conformance check on assignment: a class is assignable to
  an interface only when its member shape covers every required member.
  `isinstance(x, Interface)` is rejected unless the interface opts in via
  `@runtime_checkable`. Recursion / signature-compatibility refinement is
  deferred to Phase 4+.
- ✅ **`unsafe`** block keyword — lowers to `if True:` so scoping survives
  the Python round-trip. The type checker tracks `Checker::unsafe_depth`
  and suppresses type-mismatch / nullable-use / interface-isinstance /
  wrong-arg-count / not-callable / non-exhaustive-match diagnostics inside
  the block so users can interface with untyped Python without fighting the
  checker. Errors on lines outside the block are unaffected.
- ✅ **Pure-function detection** with the six-condition rule (sync, no `raise`,
  no `try`, no I/O builtins, no entropy/clocks, no writes to module-level
  `mut` state). `@pure`, `@memo`, and `@pure(memo=True)` decorators trigger
  the check; violations are hard errors. Memoised functions get
  `@functools.cache` injected at desugar time; `@pure`/`@memo` markers are
  stripped because they are not real Python names.
  `[strictness] auto-memoise = true` opts every passable function in.
- ✅ **`gather`** block lowers to `asyncio.TaskGroup` by default (cancels
  siblings on first failure). `gather(strategy="best-effort"):` lowers to
  `asyncio.gather(..., return_exceptions=True)`. `import asyncio` is
  injected by the desugar pass when the lowered code references it.
- ✅ **`go`** lowered through `typhon_runtime.tasks.spawn(...)` with a
  strong-ref task registry (never a bare `asyncio.create_task`).
  `go f(x) -> fut` binds the task handle.
- ✅ **Lazy imports** — `lazy import np = numpy` lowers to a thread-safe
  proxy class generated inline by `expand_lazy_imports` (double-checked
  locking, no runtime helper dependency). `lazy from x import …` is rejected
  because it defeats deferral. Module-level `lazy let NAME: T = expr` lowers
  to a sentinel-cached `lazy_let(lambda: expr)`; class-body `lazy let` lowers
  to `@cached_property`. Both round-trip through `tyc fmt`.
- ✅ **Pipe operator** `a |> f |> g(arg)` lowered to `g(f(a), arg)` left-
  associatively. Guards in `match` cases pass through to Python directly.
- ✅ **`extend`** keyword for adding methods to user-defined classes
  (alias for `impl`) and for the recognised Python built-ins (`str`,
  `list`, `dict`, …). Built-in extensions are extracted to module-level
  free functions `__typhon_ext_<TYPE>__<METHOD>` at desugar time, and
  call sites are rewritten when the receiver has a matching static
  annotation. No monkey-patching of built-ins.
- ✅ **`.dty` stub files** with `.pyi` interop emission — every `.dty` next to
  the project is compiled to a PEP 561 `.pyi` (function/method bodies become
  `...`, plain `Assign` is dropped, annotated fields are kept). `tyc check
  --stubs` parses every `.dty` and diffs its surface API (functions, classes,
  methods, annotated fields, parameter shapes) against the sibling `.ty`/`.py`
  implementation, emitting `tyc::stub_mismatch` diagnostics for
  missing-in-impl / missing-in-stub / signature-mismatch findings. A runtime
  introspection probe (mypy's `stubtest` proper) is still a follow-up.

At the end of Phase 3, Typhon is useful for a real backend or CLI project.
Everything beyond is polish and ambition.

## Phase 5.5 — Constructor / method arity safety ✅ complete (v0.2.0)

Shipped in [v0.2.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.2.0):

- **Constructor arity (`tyc::arg_count`):** the auto-generated
  `__init__` of `class` / `model` declarations is now arity-checked at
  every call site. `ApiClient(base_url="…")` for a class with a
  required `api_key: str` field is rejected at `tyc check` / `tyc
  build` time instead of failing at runtime with `TypeError: missing
  1 required positional argument`.
- **Method arity (`tyc::arg_count`):** `impl` methods now carry full
  `ArityInfo` on their `MethodSig`, so `u.greet()` is flagged when
  `greet` declares a required `prefix: str` parameter. Previously
  method calls fell into the permissive arity shape.
- **Cross-module shape propagation:** `from foo import ApiClient` and
  `import foo as f; f.ApiClient(…)` both flow through the new arity
  checks. `.ty` source and `.dty` stubs participate on equal footing,
  with stubs winning on name collisions. Works in `tyc check`, `tyc
  build`, and the LSP (which caches per-file shape extraction via a
  new salsa-tracked `module_shapes_query`).
- **Post-construction field-init audit (`tyc::missing_field_init`):**
  catches the `X.__new__(X)` / `object.__new__(X)` bypass patterns.
  If the constructed instance escapes the function (return / call
  argument) with required fields unassigned, the audit fires. Dropped
  conservatively on `setattr`, method calls, and inside `unsafe:`.

Limitations carried forward (tracked in
[`docs/findings.md`](findings.md)):

- ~~Dotted-attribute annotations (`let c: f.Cls = …`) don't resolve to
  the foreign class shape~~ — fixed in the Phase-5.2 drift sweep.
  `Expr::Attribute` annotations now produce a qualified
  `Class("{module}.{attr}")` that unifies with the call site's
  `import foo as f; f.Cls(...)` inference; downstream method-arity and
  kwarg checks fire correctly. See test
  `annotation_dotted_attribute_resolves_to_class` in `tyc-types`.
- ~~The post-construction audit doesn't track container-literal escapes
  or outer-scope assignment escapes~~ — fixed by adding an
  `audit_check_escape` hook on the RHS of every annotated assignment
  (`Stmt::AnnAssign`). A partial instance flowing into
  `let configs: list[Config] = [c]` or
  `let alias: Config = c` now fires `tyc::missing_field_init` at the
  assignment site. The target name is excluded from the check so
  `let c: T = ApiClient(...)` (rebinding the tracked name) doesn't
  false-positive on its own LHS. The audit is intra-procedural for
  the general case.
- ~~Cross-function tracking would need a richer summary IR~~ — fixed
  for the trivial factory-helper shape. A pre-scan recognises
  `def make(): return X.__new__(X)` (and the two-statement variant
  `obj = X.__new__(X); return obj`); call sites `let c = make()`
  register the LHS as tracked and a downstream escape fires
  `tyc::missing_field_init` as if the user had constructed the
  partial instance inline. Helpers that do any intervening field
  assignment are treated as initialising properly and are not
  recorded. General inter-procedural data-flow remains future work.
- Subclass constructors used to reject inherited fields
  (`Dog(name="Rex", breed="Husky")` for `class Dog(Animal):`) — also
  fixed in the same sweep via a new `effective_class_shape` helper that
  walks the inheritance chain. See tests `ctor_subclass_*` /
  `ctor_grandchild_inherits_through_chain` in `tyc-types`.

## Phase 5 — Interop and developer experience ✅ complete (v0.1.6)

Shipped in [v0.1.6](https://github.com/CodeHalwell/Typhon/releases/tag/v0.1.6):
the `plain class` marker, auto-skip for `Enum`/`Flag`/`ABC` parents plus a
user-configurable `[emit] skip-decoration-bases` list, `class-default`
validation, `or`/`and` truthy-union typing, generator→`Iterable` conformance,
`tyc explain <code>` / `tyc cheatsheet`, an upgraded `tyc init` scaffold,
`.py`-in-`src/` copy-through, `tyc build --check`, the
`tyc::contains_secret_literal` lint, miette `url(...)` deep-links on every
diagnostic with 50+ catalog pages under `docs/diagnostics/`, `tyc fmt`
wrapping `ruff format`, and `tyc debug --break <ty>:<line>` source-mapping.
The full per-section status is recorded under each heading below.

Phase 4+ is the "beyond v1" feature list. Phase 5 is the **friction
list**: real adopters land on Typhon, hit the same handful of papercuts
in the same order, and route around them. The pre-existing Phase-5
deferrals (the AST-based reprinter and the source-mapping debugger)
fold in here. Items are roughly ordered by how much they hurt; severity
shown as `pain`, `gap`, `dx`, `dx-doc`.

### 5.1 Class-emission reform — `pain` ✅

`class-default = "dataclass"` slaps `@dataclasses.dataclass(slots=True)`
on every class. This breaks any framework that sets attributes
dynamically (Textual, Pydantic v1, ORMs, metaclass-driven libraries)
and silently breaks cooperative `__init__` chains because the generated
`__init__` never calls `super().__init__()`. The only escape today is
to write a counter-intuitive `@dataclasses.dataclass(init=False,
slots=False, repr=False, eq=False)` decorator that *suppresses* tyc's
own decorator. The semantics read as "I want this to be a dataclass"
but mean "leave my class alone."

Deliverables:

- **Plain-class marker.** Reserve `plain class X:` (preferred —
  symmetric with `frozen class X:`) *or* `@plain` (rejected as
  unknown name today) for "regular Python class semantics, no
  dataclass decoration." Document as the canonical Python-interop
  escape hatch.
- **Auto-skip when subclass isn't dataclass-friendly.** When a class
  inherits from a known non-dataclass parent (`Protocol`, `Enum`,
  `BaseModel`, `App`, `NamedTuple` already handled, plus user-marked
  base classes), emit a plain class instead of a dataclass. Pair with
  a list of "skip-decoration" base classes in `typhon.toml`.
- **Validate `class-default` values.** Today `class-default = "plain"`,
  `"regular"`, `"struct"`, `"none"` are silently identical to
  `"dataclass"`. Reject unknown values with `tyc::invalid_config_value`
  and validate at load time.
- **Document the existing escape hatch.** The
  `@dataclasses.dataclass(init=False, slots=False, ...)` no-op pattern
  goes in `docs/guides/05-classes-and-models.md` as a transition
  recipe, then is superseded by the plain-class marker.
- **Together with strict typing**, `class-default = "dataclass"` makes
  Python interop painful out of the box. The pitch "stricter superset
  of Python" should hold for the standard Python idioms in the top-100
  PyPI packages without ceremony — track this as the project-level
  success metric for Phase 5.

### 5.2 Python-semantic alignment in the type checker — `pain` ✅ (two confirmed cases fixed)

The "stricter superset" promise breaks when the type checker rejects
expressions that CPython evaluates without complaint. Two confirmed
cases:

- **`x or y` typed as `bool`.** Python's `or` returns the truthy
  operand, not a bool. `let chunk: str = update.text or ""` rejects
  with `expected str, found bool`. The fix is to type `or` / `and`
  results as `Union[lhs_truthy_type, rhs_type]` (and the falsy dual
  for `and`). Same for `not x` returning a structural bool. Diagnostic
  message also needs softening — current text claims a mismatch that
  doesn't exist at runtime.
- **`Generator[T, None, None]` ↔ `Iterable[T]`.** A generator function
  satisfies `Iterable[T]` at runtime. Refusing the conformance forces
  users to rewrite `def compose(self) -> ComposeResult: yield ...`
  into `-> list[Widget]: return [...]`. Teach the conformance check
  that any generator type is structurally assignable to
  `Iterable[T]` / `Iterator[T]` / `AsyncIterable[T]` / `AsyncIterator[T]`.

These are the two found so far; the broader audit (a *Python-semantic
regression sweep*) is the Phase 5 deliverable. Each accepted-by-Python
shape that Typhon rejects becomes a `tyc::python_semantic_drift`
warning during the audit and a `pain`-level fix afterward.

### 5.3 Discoverability — `dx` ✅

Adopters today learn `mut`, `impl`, `interface`, and the class-default
opt-out by running `tyc migrate` on hand-written Python, by brute-forcing
class-declaration keywords, by reading diagnostic bodies, or — worst
case — by `strings tyc | grep`. `tyc init` scaffolds a 5-line hello-
world with no class, no methods, no `impl`, no `mut`. There is no
`tyc explain`, no built-in cheat sheet, no docs link in `tyc --help`.

Deliverables:

- **`tyc init` scaffold upgrade.** The generated `src/main.ty` includes
  a class with methods in an `impl` block, a `mut` binding, and a
  `Result` example. The generated `typhon.toml` has every `[strictness]`
  / `[emit]` key present with comments (especially `class-default`).
- **`tyc explain <code>`** subcommand prints the catalog entry for a
  diagnostic code with a short example and the canonical fix. Mirrors
  `rustc --explain`.
- **`tyc cheatsheet`** (or `tyc lang`) prints the 30-second cheat sheet
  from the skill / docs to stdout.
- **`tyc --help` footer** links the docs site, the language reference,
  and explicitly mentions `tyc lsp` (most users will discover the LSP
  via an editor plugin and never know the underlying binary).
- **Promote `tyc migrate`.** It's the single best documentation tool
  Typhon has — every keyword surfaces by example on real Python input.
  Mention it in `tyc --help`, in the README quickstart, and in the
  scaffolded `typhon.toml` comment block.

### 5.4 `.py` interop in build output — `gap` ✅

Today, dropping a `helper.py` into `src/` lets `.ty` files import it
for type-checking, but `tyc build` doesn't copy it to the output
directory. The runtime then can't find it. This closes off the
obvious escape hatch — "write the troublesome class in plain Python
and import it" — exactly when class-emission reform (5.1) isn't done
yet.

Deliverables:

- **Copy stray `.py` files in `src/`** to the build output verbatim.
  Honour the same exclusion rules as `tyc check` (skip `tests/`,
  `__pycache__/`, etc.).
- **Diagnostic when the import points at a sibling `.py` that won't
  be copied** (e.g. a relative import in a non-standard layout).
  `tyc::orphan_py_import` warning.

### 5.5 Diagnostic deep-links — `dx-doc` ✅

The Rust-style diagnostics are the project's strongest UX. The gap is
that the *first* time a user sees `impl ChatApp:` referenced inside a
warning, they have nothing to read about what `impl` is. Adding a
diagnostic-specific docs URL to every `tyc::CODE` (via `miette`'s
`url(...)` attribute or a CLI footer) closes the loop:

```
warning[tyc::method_in_class_body]: method 'compose' defined inside …
  see https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/method_in_class_body.md for the full pattern
```

Deliverables:

- Per-diagnostic URL in the diagnostics catalog, surfaced in the
  rendered miette report and in `tyc explain <code>`.
- One-page-per-code docs site section (or anchored sections in the
  language reference).

### 5.6 Build UX papercuts — `dx` ✅

- **`tyc build --check`** dry-run, mirroring `tyc fmt --check`. Lists
  which output files would be created or overwritten; no writes.
- **`class-default` validation.** Covered in 5.1 but worth restating —
  any unknown value should fail config load, not silently behave like
  `"dataclass"`.
- **Comptime env-var template substitution.** Today `env("NAME")`
  resolves at build time; extend `comptime let` to allow string
  interpolation against env so secrets/config can be marked obviously
  at the source level (e.g. `comptime let API_KEY: str = env("API_KEY")`
  with build-time substitution and a `tyc::contains_secret_literal`
  lint that flags emitted plain-text occurrences).

### 5.7 The two existing Phase-5 deferrals — `gap` ✅ (pragmatic v1)

These were called out as "Phase-5" in earlier docs and remain here for
completeness:

- **`tyc fmt` — AST-based reprinter** (`tyc/crates/tyc-format/src/lib.rs:17`,
  FINDINGS #18 / #65 / R3.15). The v1 formatter is whitespace and
  bracket spacing only. Spacing around `:`, `=`, `->` is left alone
  because it needs bracket-depth awareness (slice vs annotation). The
  Phase-5 version is a Typhon-aware printer wrapped in `ruff format`,
  with the configuration and the `--check` flag plumbed through.
- **`tyc debug` — Typhon-native source-mapping debugger.** The v1
  command is a thin wrapper around `pdb` over the emitted Python. The
  Phase-5 version reads `.py.map` to map breakpoints and steps back
  through to `.ty` source so users debug in Typhon, not in lowered
  Python.

### What's already great — keep it

Crediting the parts of the experience that buy goodwill, so they don't
regress under the Phase 5 churn:

- The build pipeline is fast — parse → check → desugar → emit → format
  in <100 ms for a real project.
- The source-map (`.py.map`) story for tracebacks is well thought out
  and `tyc trace` lands traceback frames on the original `.ty` lines.
- Diagnostic prose: `cannot assign to immutable binding 'x'`,
  `method 'compose' defined inside 'class Foo:' body — methods live
  in 'impl Foo:'`, parse errors with exact byte ranges. The Rust-
  influenced style is unambiguously better than mypy / ruff / pyright
  in places. Phase 5 should layer documentation links on top of these,
  not rewrite them.

## Phase 4+ — Beyond v1

- ✅ **Automatic `asyncio.gather` inference** (conservative). Runs of two or
  more independent `name = await callee(...)` statements inside an
  `async def` are folded into an `asyncio.TaskGroup` block when the callee
  is a `@gatherable` `async def` — declared in the same module **or imported
  from another project module** (v0.14.2) — and the awaits are statically
  independent. Opt-in via `[strictness] auto-gather = true`. The desugar
  pass injects `import asyncio` if missing. A default-on
  `tyc::gather_opportunity` advice (v0.14.2) surfaces the same independent
  runs for *any* awaited callee (including imported method calls) so the
  concurrency win is visible even without the opt-in.
- ✅ **Loop parallelisation for pure list / set / dict comprehensions**.
  `tyc/crates/tyc-analyse/src/parallel.rs` rewrites `[f(x) for x in xs]`,
  `{f(x) for x in xs}`, and `{k: f(v) for k, v in items}` into
  `typhon_runtime.parallel.map_pure(...)` (the set-comp variant wraps
  in `set(...)`; the dict-comp variant uses a dict-literal unpack to
  avoid shadowing). Opt-in via `[strictness] auto-parallel`.
  `for x in xs: out.append(...)` accumulator loops remain future work.
- ✅ **Richer comptime**. `comptime` functions and types-as-values both
  ship in v0.5.0. New `ComptimeValue::Type(String)` variant lets
  `comptime let T: type = int` round-trip through the comptime
  evaluator; bare-name resolution covers `int`, `str`, `bool`, `float`,
  `bytes`, `None`, `type`, `object`. `Any` is rejected unless imported
  because the emitter cannot synthesise the import.
- ✅ **PGO via `tyc profile`**. When `[strictness] pgo-memoise = true`,
  `tyc build` loads `typhon-profile.json` from the project root and
  promotes every `@pure` function whose observed call count meets
  `pgo-min-calls` (default 100) to `@functools.cache`, even when the
  user did not write `@memo`. Complements `auto-memoise` (which caches
  every pure function regardless of profile data). Missing profile
  file is not an error — PGO is best-effort.
- ✅ **LSP completions and code actions**. `textDocument/completion`
  returns visible bindings (walking the cursor's enclosing scope chain),
  Typhon keywords (`let`, `mut`, `gather`, `go`, `lazy`, …), and a
  small set of common Python builtins; the LSP client filters by prefix.
  `textDocument/codeAction` offers a "Remove unused import" quick-fix
  for every `tyc::unused_import` diagnostic in range. Cross-file
  go-to-definition across `.ty` / `.py` boundaries via source maps is
  still pending the v2 source-map format.
- ✅ **Migration tooling from typed `.py` to `.ty`**.
  `Optional[T]` → `T?`, dataclasses → Typhon classes,
  `@dataclass(frozen=True)` → `class X frozen:`, `class X(Protocol):` →
  `interface X:`, `NewType("X", T)` → `newtype X = T`, `TypeVar` +
  `Generic[T]` → PEP 695. See `tyc/crates/tyc/src/commands/migrate.rs`.
  The third-party-corpus sweep at
  `stress/third-party-py-corpus/` round-trips representative
  fixtures through `tyc migrate` + `tyc check` in CI
  (`third_party_corpus_round_trips_cleanly`).
- **`ty` integration** as a complementary second-stage checker over the
  desugared Python. Planned in two phases:
  - ✅ Phase 1: subprocess invocation of `ty check` via `tyc ty`, with
    diagnostic attribution via the `.py.map` source maps so `ty`'s
    `path.py:LINE[:COL]` references render as `path.ty:LINE[:COL]`.
    Pass `--raw` to opt out. No dependency on the Ruff vendor. v0.5.0
    extends the remapper's path scanner to handle paths with spaces
    (longest-candidate lookup against the `.py.map` registry).
  - Phase 2 (deferred): embedded library sharing the Salsa db. See
    [docs/ty-integration.md](ty-integration.md) for the full plan.

- ✅ **Native debugger UI translation**. `tyc debug` overrides the
  pdb subclass's `do_list`, `do_where`, `format_stack_entry`, and
  `prompt` so the entire debugger surface reads `.ty` paths and
  source slices instead of the emitted `.py`. Source-snippet
  rendering (`list`) reads the `.ty` file slice when a `.py.map`
  resolves the source path. Shipped in v0.5.0.

- **Higher-Kinded Types (HKT)**. v0.5.0 adds the foundation:
  `Type::TypeConstructor { name, arity }` represents type
  constructors with unbound parameters, and `type_from_annotation`
  recognises `F[_]` parameter syntax in class / function generic
  parameter lists. The full unification surface (constructor
  application, kind inference, variance under HKT) is staged on
  this scaffold but not yet wired into `bind_typevars_and_substitute`.
  See [`TYPE_SYSTEM_FRONTIER.md`](../TYPE_SYSTEM_FRONTIER.md) for the
  deferred work.

## Scope-cutting rule

The minimum-viable Typhon is **non-null types + sealed unions + `Result` + dataclass emit**. That alone is publishable. Everything else can be sacrificed to ship.

## Concrete next steps

Phases 0–3 are complete. Phase 5 — interop and developer experience —
shipped in v0.1.6. Phase 4+ work (everything not on the headline path):

1. ✅ **Corpus round-trip sweep.** Three CI-gated tests in
   `tyc/crates/tyc/tests/pipeline.rs`, plus the nightly opt-in PyPI
   harness:
   - `corpus_examples_all_check_clean` walks every `.ty` file under
     `examples/` and asserts `tyc check` exits 0 on each.
   - `third_party_corpus_round_trips_cleanly` walks every `.py` file
     under `stress/third-party-py-corpus/` and asserts the full
     `tyc migrate` → `tyc check` chain succeeds (covering the
     dataclass, Protocol, NewType, and PEP 695 rewrites).
   - `stress/pypi-sweep/sweep.py` (opt-in nightly) pip-installs a
     curated set of typed PyPI packages into a tempdir and round-
     trips them through `tyc migrate` + `tyc build`, then
     semantic-diffs smoke-script output against `python -m foo.bar`.
     Default config: `attrs`, `click`, and a small Pydantic-using
     package; results land in the sweep's `findings.md`.
2. **Promote `bind_typevars_and_substitute` into a proper structural
   sub-type checker that handles variance and bounded higher-kinded
   forms.** v0.5.0 adds the HKT scaffolding (`Type::TypeConstructor`
   variant + `F[_]` param syntax recognition); what remains is the
   unification piece — wiring the constructor through
   `bind_typevars_and_substitute` so `F[A]` against `list[int]`
   actually binds `F = list, A = int`. Variance inference on
   user-declared generics is still future work (today's default is
   invariant). The `generic_param_variance` table now covers the
   common heads (list / dict / Mapping / Callable / tuple / Sequence
   / Iterable / KeysView / ValuesView / ItemsView /
   AsyncContextManager / Type / Counter / …) and the bounded
   type-parameter check at the call site already dispatches through
   `is_assignable`, which honours structural conformance when the
   bound is an interface.
3. ✅ **Salsa boundary.** `preprocessed_text`, `preprocessed_full`,
   `resolved_module`, `module_decl_names`, `check_diagnostics`, and
   `module_shapes_query` are all `#[salsa::tracked]`. The
   `check_file_with_imports` path (previously the bypass) now
   delegates to a new `check_source_file_with_imports` entry that
   takes a `SourceFile` handle and consumes the tracked
   `preprocessed_full` + `resolved_module` outputs, so the LSP's
   per-keystroke cross-module check hits the parse + resolve cache
   on every unchanged sibling. Only the type-check (which depends on
   the per-invocation cross-module shape registry) actually runs
   again.
4. ✅ **Loop parallelisation for pure list / set / dict
   comprehensions.** `tyc/crates/tyc-analyse/src/parallel.rs`
   rewrites `[f(x) for x in xs]`, `{f(x) for x in xs}`, and
   `{k: f(v) for k, v in items}` into
   `typhon_runtime.parallel.map_pure(...)` when `f` is pure, opt-in
   via `[strictness] auto-parallel`. Combine with `[python]
   free-threaded = true` for real parallelism.
5. ✅ **Broaden the `tyc::python_semantic_drift` audit (round 4).**
   Closed so far: `or`/`and` truthy-union, `Generator → Iterable`,
   `bool ⊆ int` (assignment + arithmetic + unary), fixed-arity tuple
   covariance, foreign-class BinOp no-over-promote, container-
   literal escape detection. Four audit rounds catalogued — three
   in `stress/round-2026-05-23-drift/`, the fourth in
   `stress/round-2026-05-23-drift-round-4/` (17 fresh probes
   covering walrus-in-comp, augmented-assignment narrowing, `yield
   from`, match `*` capture, `raise X from Y`, `f(*args, **kwargs)`
   unpacking; all probes accept). The larger third-party corpus
   sweep is now the opt-in nightly `stress/pypi-sweep/`.
6. ✅ **A Typhon-native source-mapping debugger** that drives
   breakpoints directly against `.ty` source instead of through
   `--break TY:LINE` translation on top of `pdb`. `tyc debug`
   now writes a one-shot Python wrapper that subclasses `pdb.Pdb`,
   loads every `.py.map` sidecar at startup, and overrides
   `print_stack_entry` so every pause (entry, breakpoint, step,
   exception) prints `[ty] <src>:<line>` next to the standard `.py`
   frame. `--break TY:LINE` translation still drives the breakpoints
   themselves. Pass `--raw-pdb` to opt out and launch
   `python -m pdb` directly.
7. **VM performance plan — Tier 1 landed.** `tyc run`'s tree-walking VM
   measured ~5–18× slower than `tyc build` + CPython at steady-state
   compute (startup-adjusted) at the start of this plan. Tier 1 — a
   small-int fast path, a per-class method cache, a direct method-call
   path, and slot-resolved locals — has since landed, compressing that
   to roughly 3–14× startup-adjusted (~2.7–6× end-to-end wall clock);
   loop-shaped code gained the most, recursive call-heavy code (`fib`)
   the least. A bytecode compilation tier (Tier 2, where rough CPython
   parity on most code becomes realistic) is designed but not started.
   See [`docs/vm-performance-plan.md`](vm-performance-plan.md) for the
   measured baseline, the Tier 1 measured-outcome table, the root-cause
   breakdown, and the full tiered plan.
8. ✅ **`[optimise]` config profile + `tyc build -O`.** A single
   project-wide `level` dial in `typhon.toml`: `level = 1` flips the
   *default* of `auto-memoise`, `auto-gather`, `auto-parallel`, and
   `pgo-memoise` to `true` (an explicit `[strictness]` entry for any
   of the four always wins). `tyc build -O` / `--optimise` applies
   `level = 1` for a single invocation without editing the config.
9. ✅ **Performance-advice lint family.** Seven new advice-level
   lints living in `tyc-analyse/src/perf.rs`, gated by
   `[strictness] suggest-perf` (default on), surfaced by `tyc check`
   / `tyc build` / the LSP: `perf_membership_in_loop`,
   `perf_list_shift_in_loop`, `perf_str_concat_in_loop`,
   `perf_sort_in_loop`, `perf_sorted_first`, `perf_keys_membership`,
   and `lazy_import_opportunity`. Each fires only on unambiguous
   local AST evidence, so the example/stress corpus stays
   advice-noise-free.
10. ✅ **Free-threading parallelisation wave.** For
    `[python] free-threaded = true` projects: `auto-parallel`
    widened to cover comprehension filters, multi-argument calls,
    and nested pure calls; a new integer accumulator-loop reduction
    (`auto-parallel-reductions`) that folds a provably-bounded
    `for x in xs: total += EXPR` into a parallel `sum(map_pure(...))`
    when `total` is a plain `mut ...: int`; two new advice lints
    (`parallel_opportunity`, `shared_mut_across_tasks`); and a
    `[strictness] parallel-backend = "interpreters"` option that
    tries a PEP 734 `InterpreterPoolExecutor` before falling back to
    the thread pool. See `tyc-analyse/src/parallel.rs`,
    `reductions.rs`, and `parallel_lints.rs`.
11. ✅ **Native PEP 810 lazy imports on Python 3.15 targets.**
    `[python] target = "3.15"` / `"3.15t"` lowers `lazy import ALIAS
    = MODULE` to CPython 3.15's native `lazy import MODULE as ALIAS`
    statement instead of the `typhon_runtime` helper call — 3.13/3.14
    output is byte-for-byte unchanged.
