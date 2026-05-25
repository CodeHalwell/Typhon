# Changelog

All notable changes to Typhon are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) loosely; the
canonical phase-by-phase status lives in `docs/roadmap.md`.

## Unreleased

Carry-over sweep from the Round-3 apps-feedback campaign. Targets the
remaining ergonomics gaps that survived v0.6.0 / v0.6.1.

### Fixed — compiler

- **`tyc-resolve`: declare-only `let NAME: T` (no initialiser) is now
  allowed and integrates with sibling-arm assignment (R3-8 /
  supersedes FINDINGS #91 for the `let` shape).** The
  declare-then-assign-in-arms idiom — `let loaded: Cfg; match _load():
  case Ok(v): loaded = v; case Err(e): return Err(e); …` — previously
  fired `tyc::missing_initialiser`, forcing the user to either
  declare-as-`mut` (losing immutability) or extract a `_load_or_default`
  helper purely to give the binding an initialiser at the declaration
  site. The resolver now tracks each uninitialised `let`-declaration's
  span; the FIRST subsequent assignment to that name silently succeeds
  (it IS the initialiser), and the standard `tyc::immutable_assign`
  fires on any SECOND assignment.
  Sibling `match` arms and sibling `if` / `elif` / `else` bodies each
  count as a separate first-assignment path — the resolver snapshots
  the uninit-span set per arm and unions the initialisations
  afterwards, so the natural `case Ok(v): loaded = v / case Err(_):
  loaded = default()` and `if cond: x = a / else: x = b` shapes check
  clean while retaining `let` immutability outside the branching
  region.
  `mut NAME: T` without an initialiser is also accepted, and follows
  the usual `mut` semantics (any number of subsequent assignments are
  legal). Full definite-assignment analysis (every reachable read is
  preceded by an assignment on all CFG paths) is a follow-up; the
  minimal form here unblocks the workaround R2-7 / R3-8 documented.
- **`tyc-types`: `with cm() as r:` / `async with cm() as r:` now types
  `r` from `__enter__` / `__aenter__` methods and from
  `@contextmanager` / `@asynccontextmanager`-decorated generator
  factories (R3-3).** Previously `r` was bound to `Type::Unknown` in
  every shape, so `r.no_such_attr` and `r` flowing into a typed slot
  both slipped past the checker (Unknown is assignable to anything).
  Three lookup paths in priority order:
  1. **Decorator-aware yield-type inference** — a function decorated
     with `@contextmanager` or `@asynccontextmanager` is pre-scanned
     for its first `yield` expression; calling that function in a
     `with` / `async with` head binds the as-target to the yield
     type. Covers the canonical
     `@asynccontextmanager async def session() -> AsyncIterator[Session]:
     yield Session(...)` factory shape that all five Round-3 apps
     wanted to use but had to type-erase to keep checking.
  2. **Concrete-class `__enter__` / `__aenter__`** — a user-defined
     `class Lock:` with `def __enter__(self) -> Lock: return self`
     now propagates the method's return type to `r`.
  3. **Fall-through to `Unknown`** when neither path resolves, so the
     stdlib `with open(p) as f:` shape (whose `__enter__` lives behind
     a stub layer that doesn't carry annotations today) keeps its
     permissive behaviour. No regressions in the apps.

  The decorator pre-scan only resolves literal / `Class(...)`
  constructor / `None` yield payloads — yielding a local binding
  (`s = Session(); yield s`) still leaves `r` as Unknown because the
  pre-scan runs before the body's local-typing pass. Authors can work
  around with a re-annotation inside the body
  (`let typed: Session = r`); a full fix would re-infer the yield at
  the consumer site with the factory's local env.
- **`tyc-types`: `await` on a `Callable[..., Awaitable[T]]` /
  `Callable[..., Coroutine[Y, S, T]]` call now unwraps to `T`
  (R3-1).** The canonical async-middleware shape across
  FastAPI / Starlette / aiohttp is `next: Callable[[Req],
  Awaitable[Resp]]`; calling `next(req)` infers to `Awaitable[Resp]`
  from the `Callable` return position, and `await next(req)` should
  consume the awaitable and produce `Resp`. Without the unwrap, the
  natural shape `let r: Resp = await next(req)` failed with a spurious
  `tyc::type_mismatch: expected Resp, found Awaitable[Resp]` even
  though the runtime behaviour is correct — and the 14-api-gateway
  app had to abandon the recursive `next`-style middleware chain in
  favour of a sync `pre_hook` / `post_hook` pipeline, losing the
  ability to wrap async sections around inner handlers. The single
  biggest Round-3 finding. The canonical `async def f() -> T:` path
  is unaffected: the checker already tracks async functions as
  returning `T` directly (not `Awaitable[T]`), so the new
  unwrap-on-await arms only fire when the wrapper actually appears.
- **`tyc-types`: same-newtype arithmetic preserves the newtype, and
  one-sided literal arithmetic likewise (R2-12).** `newtype LogIndex
  = int` previously inferred `last_idx + 1` and `LogIndex + LogIndex`
  to `Type::Unknown` because the conservative numeric arm of `BinOp`
  inference only matched `(Int, Int)` / `(Float, Float)` — every
  newtype operand fell through and the result silently widened away
  the nominal tag. Raft commit-index advance, ELO updates, watermark
  math, byte offsets, and log indices all paid the same 5–10% LoC tax
  in `int(...) → Wrap(...)` round-trips just to get the types back.
  The new carve-out preserves `LogIndex` across `+ - * // % **` for
  `LogIndex + LogIndex` and `LogIndex + <literal of base>`; `/` keeps
  the existing widening to `float` because Python's `/` is always true
  division. Two **distinct** newtypes with the same numeric base
  (`LogIndex + Term`) fire the existing `tyc::operator_type_mismatch`
  diagnostic — the whole point of `newtype` is that cross-axis math
  must be opted in to. Six new `newtype_arith_*` regression tests
  guard the rule.

Polish release on top of v0.6.0. Tightens the VS Code TextMate grammar
around `let`-binding annotations and reorganises the build output
directory so emitted `.py` artefacts no longer interleave with their
`.py.map` sidecars.

No previously-accepted program changes behaviour. Existing build dirs
written by v0.6.0 keep working — every map-consumer (`tyc trace`,
`tyc debug`, `tyc ty`) falls back to the legacy adjacent layout when
no `.sourcemaps/` subtree is present.

### Fixed — vscode (`editors/vscode` 0.1.8 → 0.1.9)

- **`let name: lowercase_type = …` now highlights as a type annotation.**
  The standalone `binding-declaration` rule was a one-shot `match` that
  stopped at the binding name, leaving the `:` and the type to fall
  through to the shared `type-annotation` rule. That rule rejects bare
  lowercase identifiers on purpose (to avoid mis-scoping `if cond:`
  and `case Foo(x):`), so `let now: datetime = datetime.now()` painted
  the colon as a generic separator and `datetime` as `variable.other`
  — visibly inconsistent with `let cfg: SchedulerConfig` (uppercase →
  correct), `let n: int` (builtin → correct), and `let c:
  sqlite3.Connection` (dotted → correct). `binding-declaration` is now
  a `begin`/`end` rule that owns the `: Type` slot, routing the
  right-hand side through `type-expression`. A new Unicode-aware
  lowercase fallback in `type-expression` paints `datetime`, `asyncio`,
  and user-defined aliases as `entity.name.type.typhon`.

### Changed — build output layout

- **`.py.map` sidecars now live under `<out>/.sourcemaps/`.** Mirrors
  the emitted Python tree (`build/foo.py` → `build/.sourcemaps/foo.py.map`,
  `build/pkg/bar.py` → `build/.sourcemaps/pkg/bar.py.map`), so the
  build directory is no longer cluttered by interleaved map files.
  Map resolvers in `tyc trace`, `tyc debug`, and `tyc ty` discover the
  new location by walking up from the `.py` and explicitly prefer
  `.sourcemaps/` over the legacy adjacent layout so a stale sidecar
  left over from an upgrade can't shadow a fresh map.

## 0.6.0 — 2026-05-25

Apps-feedback minor release. A two-round stress campaign that built
ten multi-file production-shaped apps on top of v0.5.2 (event-sourced
banking, distributed key-value store, mini-compiler, search engine,
GraphQL server, game ECS, trading engine, ML orchestrator, web
crawler, task scheduler — under `examples/apps/`) surfaced a batch of
correctness and ergonomics gaps. This release closes every issue from
that campaign, adds three additive features (Result method API on
`Ok` / `Err`, `impl` distribution on sealed-union aliases, and a new
`tyc::stdlib_module_shadow` warning), and ships the apps themselves
as canonical multi-file reference programs.

No previously-accepted program changes behaviour. The new features
are strictly additive: existing free-function `result.map(...)` calls
keep working alongside the new `ok.map(...)` method form, and the
new warning is non-fatal.

### Added — language & runtime

- **`Ok` / `Err` expose the standard Result combinators as methods
  (R2-6).** `map`, `map_err`, `and_then`, `or_else` are now bound on
  the runtime classes, not just the free functions in
  `typhon_runtime/result.py`. Heterogeneous-error pipelines that
  previously had to materialise a 4-deep `match` tower with `raise
  RuntimeError("unreachable")` trailers can now normalise per stage in
  chain form:

  ```ty
  let toks = tokenize(src).map_err(_lex_to_pipeline)?
  let ast  = parse(toks).map_err(_parse_to_pipeline)?
  let ty   = check(ast).map_err(_type_to_pipeline)?
  ```

  Semantics match the standard algebra: `Ok.map` transforms the value
  while `Ok.map_err` is identity (and vice versa for `Err`);
  `and_then` chains a `Result`-returning op on `Ok`; `or_else`
  recovers from `Err`. A `build_features` integration test verifies
  the methods land in the emitted runtime and the full algebra
  round-trips through Python.
- **`impl` on a sealed-union alias distributes to every variant
  (R2-3).** `impl Event:` where `Event = A | B | …` used to fire
  `tyc::impl_unknown_class` because the impl-merger only looked for a
  single target class. The desugar pass now collects sealed-union
  type aliases and replicates the impl block's methods on every
  variant class, and the type checker mirrors the same fold so method
  lookup resolves without dropping to free functions. Replicated
  bodies retain any `match self:` patterns; per-variant dispatch is
  automatic because the runtime class on `self` only matches its own
  arm. The duplicate-method check from the concrete-class branch is
  mirrored into the union branch, so a method declared on both `impl
  Union:` and `impl Variant:` still fires `tyc::duplicate_method`.
- **`tyc::stdlib_module_shadow` warning (R2-4).** A project `.ty` file
  whose stem matches a Python 3.13 stdlib top-level module (`types`,
  `ast`, `string`, `io`, `json`, `dataclasses`, `logging`, `random`,
  `time`, …) emits `build/<name>.py`, which the default `python
  build/main.py` entry point puts on `sys.path` ahead of the stdlib.
  Transitive imports (e.g. `dataclasses` → `types`) then resolve to
  the project module instead of the standard library, producing
  baffling `ImportError`s blamed on innocent stdlib packages. The new
  warning fires per-file in `tyc check`, points at the rename pattern
  (`lang_types.ty`, `records.ty`, …), and is gated on
  `typhon.toml` — standalone-file checks skip it. Severity is `warn`
  (non-fatal); `tyc explain stdlib_module_shadow` and
  `docs/diagnostics/stdlib_module_shadow.md` are wired up.

### Fixed — compiler

- **`tyc-types`: variant→sealed-union flow now works across module
  boundaries.** A `pub type Event = A | B` declared in `lib.ty` would
  let a variant `A(...)` flow into an `Event`-typed slot within
  `lib.ty`, but a consumer module that imported both the variants and
  the alias hit `tyc::type_mismatch` on the same construction.
  Sealed-union variant tables, like interface declarations, weren't
  included in the cross-module shape extractor. Both `ModuleShapes`
  and `ExternalShapes` now carry `sealed_unions` and `interfaces`
  maps; the CLI / LSP re-key them under each local import name
  (including alias renames) so the consumer's checker sees the same
  union→variant relationship the source module sees.
- **`tyc-types`: cross-module function signatures preserve parameter
  and return types.** Functions imported via `from foo import f` were
  registered with `Type::Unknown` placeholders for every parameter
  and the return type — the `ArityInfo` sidecar carried names +
  counts but no types. A nullable parameter `def takes(p: Price?) ->
  int:` consumed from another module would render in
  `tyc::nullable_use` diagnostics as the literal placeholder `?`
  ("value is `? | None`, where `?` is required"), and silently accept
  any value the caller passed. `ArityInfo` now records `param_types`,
  `kwonly_types`, and `return_type`; the cross-module binding seed
  reads them so an imported function looks identical to a local `def`
  at call sites.
- **`tyc-types`: nullable-use diagnostic no longer renders `?` as the
  expected type (R1#11).** A companion fix to the cross-module
  signature seed: where the bound was still `Type::Unknown`, the
  formatter used to print a bare `?` for the expected type, producing
  the surreal "value is `? | None`, where `?` is required". The
  formatter now substitutes the resolved bound or, if still unknown,
  falls back to a clearer phrasing.
- **`tyc-types`: exhaustive `match` on a sealed union now satisfies
  missing-return analysis for any subject expression.** `match
  get_state(): case A(): ...; case B(): ...` against a sealed union
  returned by a function fired a false-positive `tyc::missing_return`
  because the analyser only inferred match subject types for bare
  names and attribute access. It now falls back to expression
  inference for any subject shape, so function calls, subscript
  expressions, and arbitrary expressions all flow through the
  exhaustiveness path.
- **`tyc-types`: partial keyword pattern satisfies match exhaustiveness
  (R1#9).** `case Foo(field=x):` (which binds only `field` and ignores
  the rest) was treated as non-exhaustive against the variant even
  though Python's `match` accepts it as a structural match on the
  class. Keyword patterns are now folded into the per-variant
  coverage tally the same way positional patterns are.
- **`tyc-resolve`: `let` declarations in sibling `case` arms no longer
  shadow each other.** `case A(): let key = ...; case B(): let key
  = ...` fired `tyc::no_block_shadow` even though at most one arm
  runs at runtime. The resolver now drains arm-local bindings into a
  side buffer between cases (and splices them back after the match
  for `report_unknown_names`), mirroring the per-arm `env.enter()` /
  `env.leave()` the type checker already does.
- **`tyc-resolve`: for-target no longer silently rebinds prior `let`
  bindings (R2-17).** Python's for-target is an assignment, not a
  fresh declaration, so the iteration value overwrites whatever was
  previously bound. The resolver used to trip `tyc::immutable_assign`
  against a prior `let` in the enclosing scope, even when the prior
  let was inside an unrelated sibling for-loop body. The
  sibling-for-loop case was already silenced; this release extends
  the same silencing to any prior binding when the *new* binding is a
  for / with / except / comprehension target. Manual body-level
  assignment (`i = i + 1`) is unaffected — that path goes through
  bareword-assignment resolution, not the loop-target declaration
  path.
- **`tyc-syntax`: `pub freeze let X = …` now parses.** `pub` stacks
  with every other module-level binding modifier per the diagnostic
  docs, but the `pub`-prefix stripper didn't recognise `freeze let`
  as a multi-word keyword form. `pub freeze let DEFAULT: dict[str,
  int] = {...}` now lowers correctly and round-trips through `tyc
  fmt`.
- **`tyc-syntax`: `pub def` is now visible to the `?` operator
  validator (R2-1).** The `?` propagation pass walked function
  declarations to check that the enclosing return type accepted
  `Result`, but the `pub`-prefix stripper ran after the walk so
  `pub def f() -> Result[T, E]:` looked like a bare `def` with no
  return type and the validator gave up. The stripper now runs
  upstream of the validator.
- **`tyc-types`: `loop.run_until_complete(coro())` no longer fires
  `tyc::missing_await`.** The asyncio event-loop method is a
  legitimate consumer of coroutines — same family as `asyncio.run(...)`,
  which the analyser already excluded. Added to the coro-acceptor
  whitelist; the receiver shape is unconstrained because the method
  name itself is the carve-out.
- **`tyc-types`: `@contextmanager` factory bodies are exempt from
  `tyc::resource_not_managed`.** A `@contextmanager`-decorated
  function whose body opens a file or socket *is* the resource
  manager — flagging `let f = open(path)` inside is a false
  positive. The check now skips function bodies carrying
  `@contextmanager` or `@asynccontextmanager` (bare or dotted-module
  form).

### Improved — diagnostics

- **`tyc::type_mismatch` help text now suggests widening the
  annotation, not narrowing it to the found type.** The previous help
  text — "change the value, or update the annotation to `<found>`" —
  implicitly suggested dropping the expected type, which is almost
  never what the user wants. Now reads "change the value so it
  produces `<expected>`, or widen the annotation to `<expected> |
  <found>` if both are intended", which matches typed Python's
  culture and the way users actually fix the error.

### Improved — docs

- **`docs/diagnostics/stdlib_module_shadow.md`** ships as a new
  catalog page covering the rationale, the `ImportError` cascade it
  prevents, and a rename table for the most common collisions
  (`types.ty → lang_types.ty`, `dataclasses.ty → records.ty`, etc.).
- **`docs/diagnostics/class_attr_shadows_slot.md`** gains a new
  "Alternative: nullary sealed-union variants" section pointing at
  the `pub class TyInt frozen: pass` idiom instead of the
  `placeholder: int = 0` workaround.
- **`docs/guides/07-sealed-unions-and-match.md`** now documents
  keyword patterns (`case TaskStarted(task_id=tid):`) as the
  recommended form for variants with more than two or three fields —
  positional patterns must match field count exactly, keyword
  patterns don't, and Python's `match` supports the latter natively.
- **`docs/cli.md`** documents `tyc explain --list` (which the
  diagnostic hint footers have always advertised but the command
  table never mentioned).

### Added — examples

- **`examples/apps/` — ten production-shaped multi-file apps** built
  on top of v0.5.2 as a stress harness: task scheduler, trading
  engine, ML orchestrator, event-sourced banking, web crawler,
  GraphQL server, game ECS, mini-compiler, search engine, distributed
  KV. Each ships under a `typhon.toml`, `tyc check`s clean, `tyc
  build`s, and runs through CPython. Every app carries a
  `FRICTION.md` or per-app README noting which compiler / ergonomics
  gaps the build surfaced — those gaps are the source list for the
  fixes and features above. `examples/apps/TYPHON_FEEDBACK.md`
  aggregates the campaign findings.

### Changed — VS Code extension

- **Version `0.1.7 → 0.1.8`.** No new keywords or grammar surface in
  this batch (the deep grammar audit in v0.5.1 already covers
  `pub freeze let`, sealed-union impls, the `?` operator, and the
  `lazy_let` runtime helper). The bump tracks the new tyc release.

### Fixed — LSP semantic tokens

- **`newtype` declarations now paint as `class` everywhere.** `newtype
  DocId = int` desugars to `DocId = NewType("DocId", int)`, which the
  resolver registers as a `BindingKind::Value` — same kind a plain
  `let x = 1` gets. Every reference site (`def f(id: DocId) -> DocId:`)
  was therefore emitted as the generic `variable` semantic-token type
  and rendered as the local-variable colour (light blue in Dark+,
  plain text in most user themes) while real classes like `Document`
  defined right next to them rendered as `class`. The user's
  screenshot of `examples/apps/09-search-engine/src/index.ty`
  surfaced exactly this asymmetry. `tyc-lsp::semantic` now walks the
  module for `NAME = NewType("NAME", BASE)` shapes and promotes both
  the declaration and every reference to the `class` token, matching
  how `pub class X:` declarations already paint.
- **Class-body field declarations now paint as `property` instead of
  `variable`.** `pub class Document: id: int` used to emit `id` as a
  local-variable token even though it's a dataclass slot, which made
  field names render with the local-binding colour rather than the
  property colour Pylance gives Python `@dataclass` fields. The token
  type now derives from the binding's enclosing scope kind so
  class-body Value bindings emit as `property` and function/module
  Value bindings stay as `variable`. Keeps the in-class declaration
  consistent with the `obj.field` access elsewhere in the file.

## 0.5.2

A correctness + documentation point release on top of v0.5.1. Two
compiler bugs surfaced during a comprehensive audit of the
`docs-site/` example corpus are fixed; the docs site itself gets a
~40-file sweep adding side-by-side Typhon / Emitted-Python tabs to
every complete-program example; and a re-runnable audit harness lands
under `docs-site/scripts/` so the same sweep can run in CI.

No language semantics change beyond accepting two previously-rejected
forms (`set - set` and `<ident>?` propagation), and no
previously-accepted program changes behaviour.

### Fixed — compiler

- **`tyc-syntax`: `<ident>?` in value position now lowers to the standard `__typhon_q_N__` ladder.**
  `let x: T = a?` after a `gather:` block (or any time the operand
  was a bare identifier rather than a paren-prefixed call `f()?`)
  used to be silently rewritten as `x: T = a | None`, then crash at
  runtime with `TypeError: unsupported operand type(s) for |: 'Ok'
  and 'NoneType'`. The propagation pass only recognised `f()?`
  because it required the character before `?` to be `)`. It now
  disambiguates by RHS position: when the line has an `=` and the
  identifier-then-`?` sits on the RHS, treat the trailing `?` as
  propagation and emit the standard `__typhon_q_N__` ladder. Pure
  annotation forms (`let x: int?`, `let x: list[int]?`) and type
  aliases (`type X = T?`, `newtype Y = T?`) are unchanged. Unblocks
  the natural `gather:` → `?` → `Ok(...)` shape used across the
  tour, first-program, and recipes docs.
- **`tyc-types`: `set - set` and `frozenset - frozenset` now type-check as Python set difference.**
  The operator-compatibility check for `-` accepted only numeric
  operands; the Python set-difference form (`{1, 2, 3} - {2, 3, 4}`)
  fired `tyc::operator_type_mismatch` even though the sibling bitwise
  operators `&`, `|`, `^` (which fall through to the permissive arm)
  worked. Added an explicit carve-out for `Operator::Sub` between two
  `set` / `frozenset` operands, matching the existing carve-outs for
  `+` on `list` / `list` and `tuple` / `tuple`. Closes the
  `types/collections.mdx` set-operations example.

### Changed — documentation

A comprehensive audit of the `docs-site/` example corpus rolled out
across roughly 40 `.mdx` files. Every complete, runnable Typhon
example in the tour, types, reference, recipes, getting-started, and
lowering sections is now presented as a side-by-side
`<Tabs><TabItem label="Typhon"/><TabItem label="Emitted Python"/></Tabs>`
pair, with the Python side produced by running the example through
the actual `tyc build` pipeline rather than written by hand. Partial
illustrative snippets and intentional negative examples are left as
plain `python` fences.

Stale or incorrect doc claims that the audit surfaced were also
fixed:

- **`lazy let X: T:` colon-block form (`reference/lazy.mdx`,
  `lowering/lazy.mdx`)** was shown for class-level `lazy let` but
  doesn't parse. The correct form is `lazy let X: T = expr`. The
  generator-style `lazy[T]` return-type form is documented as
  designed-but-not-yet-implemented, since the parser doesn't
  recognise `lazy` in return-type position today.
- **Multi-line `|>` pipes without wrapping parens
  (`reference/pipes.mdx`)** fail parsing the same way any multi-line
  Python expression does. Updated to use the working
  `let slug: str = (...)` parens-wrapped shape and noted the
  requirement.
- **`model X frozen:` (`interop/pydantic.mdx`)** isn't parsed — the
  `frozen` modifier is recognised on `class` only. Replaced with a
  `.py` escape-hatch example and a roadmap note.
- **`let`-shadowing (`reference/let-mut.mdx`,
  `diagnostics/binding-errors.mdx`, `tour/five-rules.mdx`)** was
  documented in two places as a supported fix for `tyc::no_block_shadow`,
  but the checker rejects every form of `let`-shadowing because
  Python is function-scoped and a nested `let x` would silently
  rebind the outer binding. Corrected to recommend `mut` or a fresh
  name.
- **`tour/control-flow.mdx`** said static fall-through analysis was
  "reserved for a future release". It isn't — the checker enforces
  `tyc::missing_return` today. Converted the example to a clearly-
  marked negative case.
- **`lowering/runtime.mdx`** was rewritten to show the exact
  emitted-runtime source as of this release: `__init__.py`,
  `tasks.py`, `lazy.py`, `freeze.py`, plus the `parallel.py` /
  `stdlib.py` / `result.py` siblings. The earlier excerpts were
  simplified and out of date.
- **`diagnostics/compile-errors.mdx`**: the lazy-let example was
  written inside the `class` body (which fires
  `tyc::method_in_class_body`); moved into an `impl` block where the
  feature actually belongs.

### Added — tooling

`docs-site/scripts/verify_examples.py` walks every `.mdx` under
`src/content/docs/`, extracts python / typhon / ty code blocks,
classifies each one (partial snippet / intentional negative /
complete program / emitted-Python block), and tries to compile every
complete-program block via `tyc build` in a temporary project.
`--real-only` filters out the partial-snippet noise that fires when a
small example references a class defined earlier on the same page.
Exits non-zero when real issues remain so this can wire into CI.

## 0.5.1

A correctness + tooling point release on top of v0.5.0. Two compiler
bugs that PR #120 reviewers caught on the new examples corpus are
fixed; the VS Code TextMate grammar gets a deep audit (PRs #119, #121)
that closes ~18 miscolorings across real Typhon code; and the
examples directory grows by 22 stdlib-only exercises (47–68) plus
emitted `.py` companions for every example shipped to date.

No language semantics change — every previously-accepted program
still compiles, and the three compiler fixes only narrow the set of
*emitted-Python* bugs the toolchain produces.

### Fixed — compiler

- **`tyc-format`: triple-quote tracker desync (PR #120).** The
  in-process whitespace pass treated `"""` as three independent
  toggles of its single-quote tracker. On a line like
  `""", encoding="utf-8")` (which `ruff format` produces when it
  reflows `"\n".join(xs) + "\n"` into triple-quoted form), the
  tracker came out of sync and rewrote `"utf-8"` as `"utf - 8"` —
  an encoding name Python rejects with `LookupError` at runtime.
  Added a triple-quote state machine that consumes everything up to
  the matching `qqq` closer verbatim, plus a regression test. Two
  PR reviewers flagged this on examples 16 and 21.
- **`tyc-desugar`: pattern-walk skipped `case Ok(...)` / `case Err(...)`
  (PR #120).** `stmts_use_result_names` walked match-case bodies and
  guards but skipped the patterns themselves, so a file that only
  ever pattern-matched on `Ok` / `Err` (without returning or
  constructing a `Result` anywhere) didn't trigger the
  `from typhon_runtime import Ok, Err, Result` auto-injection. The
  emitted `.py` then `NameError`'d at runtime — caught on
  `examples/47-mini-app/src/api.py` and previously worked around in
  `examples/testing/test_calculator.ty`. Added
  `pattern_uses_result_names` that walks every `Pattern` variant
  (`MatchValue`, `MatchSequence`, `MatchMapping`, `MatchClass`,
  `MatchAs`, `MatchOr`, `MatchStar`, `MatchSingleton`).
- **`tyc-syntax`: duplicate `__typhon_Err__` alias imports (PR #120).**
  `prepend_typhon_err_alias_import` runs once per pipeline pass that
  introduces `?` propagation or `with`-chain lowering, with a
  header-scan check meant to suppress duplicates when an earlier pass
  already injected the alias. The check compared `trimmed` (still
  carrying its trailing newline) against `IMPORT_LINE.trim_end()` (no
  newline), so equality never matched and pass 2 injected a second
  copy directly above pass 1's. Strip the trailing newline before
  comparing; added a regression test; rebuilt
  `examples/07-error-handling/error_handling.py` to confirm only one
  alias line ships.

### Changed — VS Code extension (PRs #119, #121)

The TextMate grammar got a deep audit against ~19k lines of real
Typhon code from `examples/` and `stress/` plus a hand-built corpus
exercising every language construct. Tokenisation was driven through
`vscode-textmate` + `vscode-oniguruma` and every flagged span fixed.
The extension version bumps `0.1.5 → 0.1.7`.

- **Split `expression` into `expression` + `expression-inner`.**
  Subscripts `[...]`, dict / set literals `{...}`, and lambda bodies
  now route through `expression-inner` and handle their own `:` so it
  stops being eaten as a spurious type annotation. Fixes
  `xs[1:4:2]`, `{k: v for k, v in xs}` (the comprehension `for` was
  being scoped as a type-expression identifier), and
  `lambda x: x + 1` (the body was parsed as a type).
- **Balanced `#parens` matcher in expression-inner** so nested parens
  in parameter defaults (`s: T = Depends(get_store)`) consume their
  own `)` instead of letting it close the outer parameter list.
- **Tightened type-annotation lookahead.** The colon in single-line
  statements like `if isinstance(...): return left + right` and
  `case Foo(x): bar()` no longer fires as a type annotation. The
  check now requires an uppercase identifier, a builtin type name, a
  dotted lowercase name (`pd.DataFrame`), or a type-shaping
  punctuation (`[`, `(`, quote, …) to follow.
- **~18 additional miscoloring fixes** across keyword spans
  (`pub` / `freeze` / `extend` / `unsafe`), f-string nesting, regex
  literals, `match` arms, and decorator argument lists. See PR #121
  for the full list.

### Added — examples (PRs #119, #120)

- **22 new stdlib-only exercises (examples 47–68).** Practical Typhon
  programs that emit valid Python without any third-party
  dependencies: a mini-app (47), `newtype` IDs (48), Fibonacci memo
  (49), linked list (50), BST (51), stack & queue (52), sorting (53),
  graph traversal (54), word frequency (55), state machine (56),
  iterators / generators (57), context managers (58), matrix ops
  (59), Caesar cipher (60), tic-tac-toe (61), priority queue (62),
  event bus (63), URL router (64), INI parser (65), rate limiter
  (66), trie (67), JSON-RPC builder (68).
- **Emitted `.py` companions** for every example 01–46 so readers can
  see the lowering without running `tyc build`.
- **`examples/testing/`** gains a calculator + pytest companion that
  exercises the `Result`-in-tests pattern.

## 0.5.0

The post-v0.4 roadmap-sweep release. v0.5.0 lands the seven Phase
4+ items shipped on `claude/typhon-roadmap-items-nCs4K` (PR #105)
plus four follow-up epics that close the open frontier work flagged
during that sweep (PRs #110, #111, #112, #113):

1. The Salsa boundary now caches the parse + resolve pair behind a
   single `preprocessed_full` query and `check_source_file_with_imports`
   reuses the cached diagnostics instead of double-resolving (PR #112).
2. The auto-parallel comprehension rewriter grew dict-comp support
   alongside the existing list-comp + set-comp coverage, and the
   `tyc debug` wrapper now reads in full Typhon coordinates (`where`,
   `list`, prompt, frame summary) instead of just the post-pause
   banner. `tyc ty`'s diagnostic remapper handles paths with spaces
   by walking left until a candidate path resolves to a real
   `.py.map` (PR #111).
3. `Type::TypeConstructor` and `ComptimeValue::Type` give the type
   system a foothold for higher-kinded types and types-as-comptime-
   values without committing to the full HKT surface — the parser
   accepts `F[_]` parameter syntax in class generics, and
   `comptime let T: type = int` round-trips through emit (PR #113).
4. Corpus coverage gains the PyPI sweep harness (3+ third-party
   packages round-tripped through `tyc migrate` + `tyc build` with
   semantic-diff against `python -m foo.bar`), a fourth
   `python_semantic_drift` audit round (17 fresh probes, all green),
   and a design doc for the general inter-procedural field-init
   audit that PR #105's trivial-factory work intentionally scoped
   out (PR #110).

This is a minor release because the type-system additions in (3)
expand the accepted surface — programs that previously failed parse
or type-check on `F[_]` / `comptime let T: type = ...` will now
compile. No previously-rejected runtime semantic is newly accepted.

### Added — incremental compilation

- **Salsa cache shared across `check_file_with_imports` (PR #105).**
  New `preprocessed_full` tracked query returns the full
  `PreprocessResult` so `preprocessed_text`, `resolved_module`,
  `module_decl_names`, and the new `check_source_file_with_imports`
  entry point all share one preprocess pass per revision. The LSP
  now calls the SourceFile-backed entry directly so a per-keystroke
  cross-module check hits the cached parse + resolve on every
  unchanged sibling; only the type-check (which depends on the
  per-invocation cross-module shape registry) actually re-runs.
- **Eliminated double-resolve in `check_source_file_with_imports`
  (PR #112).** `ArcResolvedModule` now carries the resolver
  diagnostics alongside the resolved module (both behind `Arc` for
  pointer-equality `salsa::Update`), so the second
  `resolve_module_with` call that previously ran just to harvest
  diagnostics is gone. Every consumer of `resolved_module` (LSP
  hover, definition, the multi-import check path) shares the same
  cached output.

### Added — `tyc ty` integration

- **Diagnostic attribution for `ty` output (PR #105).** `tyc ty` now
  captures the `ty` subprocess output and rewrites every
  `path.py:LINE[:COL]:` reference to the originating Typhon source
  via the adjacent `.py.map` sidecars. Pass `--raw` to forward
  output verbatim. The shared `commands/source_map.rs` module is
  consumed by both `tyc trace` and `tyc ty`.
- **Path-with-spaces handling in the diagnostic remapper (PR #111).**
  `parse_py_ref` now walks left from each `.py:` occurrence and
  yields successively longer candidate prefixes, taking the longest
  match that corresponds to a real `.py.map` sidecar. The lookup is
  cached per-line so the candidate enumeration stays O(1) amortised.

### Added — `tyc debug`

- **Typhon-aware pdb wrapper (PR #105).** `tyc debug` writes a
  one-shot Python wrapper that subclasses `pdb.Pdb` and prints
  `[ty] <src>:<line>` after every pause (entry, breakpoint, step,
  exception). The wrapper loads every `.py.map` sidecar under the
  build directory at startup so per-pause lookup is a dict + list
  dereference. Default-on; pass `--raw-pdb` to launch
  `python -m pdb` directly.
- **Full UI translation: `where`, `list`, prompt, stack summary
  (PR #111).** The pdb subclass now overrides `do_list`,
  `do_where`, `format_stack_entry`, and the `prompt` property so
  the entire debugger surface reads `.ty` coordinates instead of
  the emitted `.py` paths. Source-snippet rendering (`list`) reads
  the `.ty` file slice when a `.py.map` resolves the source path.

### Added — `tyc migrate`

- **Three new line-level rewrites (PR #105).**
  - `@dataclass(frozen=True[, ...])` (and `@dataclasses.dataclass`)
    drops the decorator and the following class header gains a
    trailing `frozen` modifier (`class Vec frozen:`).
  - `class X(Protocol):` and `class X(Protocol[T]):` become
    `interface X:` / `interface X[T]:`. Multi-base forms are left
    untouched.
  - `NAME = NewType("NAME", BASE)` at module level becomes
    `newtype NAME = BASE`. The matching `Protocol` and `NewType`
    `from typing import …` entries are pruned alongside.

### Added — type checker

- **Cross-function field-init audit (PR #105).** A pre-scan
  recognises the trivial factory-helper shape `def make(): return
  X.__new__(X)` (and the two-statement `obj = X.__new__(X); return
  obj` variant). Call sites `let c = make()` register the LHS as a
  tracked uninit instance using the helper's missing field set, so
  a downstream escape fires `tyc::missing_field_init` exactly as if
  the user had constructed the partial instance inline. Helpers
  that do any intervening field assignment are treated as
  initialising the instance properly and are not recorded.
- **Variance table expansion (PR #105).** `generic_param_variance`
  gains `AsyncContextManager`, `KeysView`, `ValuesView`,
  `ItemsView`, `Type` / `type`, and `Counter`.
- **Higher-Kinded Types foundation (PR #113).** New
  `Type::TypeConstructor { name, arity }` variant represents type
  constructors with unbound parameters. `type_from_annotation`
  recognises `F[_]` parameter syntax inside class / function
  generic parameter lists (e.g. `class Functor[F[_]]:`) and
  `walk_typevars` traverses the new variant. The full surface
  (`def map[F[_], A, B](fa: F[A], f: A -> B) -> F[B]`) is staged on
  this scaffold; the design doc `TYPE_SYSTEM_FRONTIER.md` records
  the deferred unification work.

### Added — analyser

- **Parallel comprehensions: set-comp support (PR #105).**
  `{f(x) for x in xs}` now rewrites to
  `set(typhon_runtime.parallel.map_pure(lambda x: f(x), xs))` under
  `[strictness] auto-parallel`. Same eligibility rules as the
  list-comp path; the `set(...)` wrapper preserves the runtime set
  semantics (uniqueness + unordered).
- **Parallel comprehensions: dict-comp support (PR #111).**
  `{k_expr: f(v) for k, v in items}` rewrites to a dict-literal
  unpack form (`{**dict(typhon_runtime.parallel.map_pure(...))}`)
  that avoids the `dict` shadowing concern from the set-comp path.
  Only fires when exactly one of `{key_expr, value_expr}` is a
  pure-call eligible side.
- **`comptime` types-as-values (PR #113).** New
  `ComptimeValue::Type(String)` variant lets `comptime let T: type
  = int` (and any in-scope type name) round-trip through the
  comptime evaluator. The bare-name resolution covers the eight
  primitive heads (`int`, `str`, `bool`, `float`, `bytes`, `None`,
  `type`, `object`); `Any` is intentionally rejected unless
  imported because the emitter cannot synthesise the import.

### Added — test infrastructure

- **Third-party Python corpus round-trip sweep (PR #105).** New
  `stress/third-party-py-corpus/` ships six representative Python
  fixtures (dataclass, frozen dataclass, Protocol, NewType, PEP 695
  generic, legacy `Generic[T]`) and the integration test
  `third_party_corpus_round_trips_cleanly` exercises the full
  `tyc migrate` → `tyc check` chain on each. Failures are collected
  so a single run surfaces the complete regression list.
- **PyPI sweep harness (PR #110).** `stress/pypi-sweep/` ships
  `sweep.py`, a CLI that pip-installs a configured set of typed
  Python packages into a tempdir, runs `tyc migrate` + `tyc build`,
  and compares smoke-script output against the original package.
  The default config covers `attrs`, `click`, and a small
  Pydantic-using package; results land in `findings.md` so
  regressions are tracked across runs. Opt-in nightly job; not
  wired into per-PR CI because the pip install dominates the
  budget.
- **`python_semantic_drift` audit round 4 (PR #110).** Fresh
  17-probe sweep in `stress/round-2026-05-23-drift-round-4/`
  covering walrus-in-comprehension, augmented-assignment narrowing,
  `yield from` generator delegation, match-pattern `*` capture,
  string multiplication, `raise X from Y` typing, and `f(*args,
  **kwargs)` unpacking. All probes accept under `tyc check`; the
  pre-existing closed set carries forward unchanged.
- **Inter-procedural field-init audit design
  (`stress/interprocedural-audit-design.md`, PR #110).** Records
  the summary-IR sketch for generalising PR #105's trivial-factory
  audit to multi-step factories that finish initialisation across a
  call chain. No code change yet — the IR design is the prerequisite.

### Docs

- **`docs/roadmap.md`** updated to mark every Phase 4+ item shipped
  in this release as ✅ complete. The HKT row records the staged
  surface (`Type::TypeConstructor` + `F[_]` param syntax) and
  references the deferred unification work in
  `TYPE_SYSTEM_FRONTIER.md`.
- **`TYPE_SYSTEM_FRONTIER.md`** (new) catalogues the type-system
  work that remains beyond v0.5.0: full HKT unification, variance
  inference on user-declared generics (currently invariant by
  default), and the broader comptime types-as-values story.
- **`stress/EPIC_SUMMARY.md`** (new) summarises the PR #110 epic
  scope — what shipped, what's still open in the inter-procedural
  audit design, and the next-round drift candidates.

## 0.4.0

A correctness sweep on the v0.3.0 surface (PR #103) plus a focused
`tyc::python_semantic_drift` audit catches the highest-impact silent
under-checks left over from the May 2026 round: `bool` is now treated
as a subtype of `int` in assignment / arithmetic / unary contexts,
fixed-arity tuples are covariant on every slot (not just slot 0),
subclass constructors finally see inherited fields, and
`tyc::missing_field_init` no longer loses tracking when a partially-
initialised instance escapes via a container literal or annotated
alias. Emit gains triple-quoted-string preservation so multi-line
docstrings stop round-tripping into single-line `\n`-escaped blobs;
types gains De Morgan narrowing so `not (A or B)` refines both names
in the post-`if` branch. Rounded out by a corpus round-trip CI gate
(every `.ty` under `examples/` must `tyc check` clean on every PR),
re-enabled upstream Ruff insta tests (266 vendored tests back online),
and a guide-10 backfill that finally documents `newtype`, `freeze let`,
and `pub` end-to-end.

This is a minor release because the type-checker behavioural changes
(bool ⊆ int, fixed-arity tuple covariance) are observable from user
code — programs that previously failed will now compile. No
intentionally-rejected program is newly accepted that would change
runtime semantics.

### Added — language

- **`bool ⊆ int` subtype widening.** CPython defines `bool` as a
  strict subclass of `int`, but the v0.3 checker rejected
  `let x: int = True`, `let x: int = 1 + True`, and `let y: int = -True`
  as `tyc::type_mismatch`. New `(Int, Bool)` and `(Float, Bool)` arms
  in `assignable`; the `BinOp` arithmetic case folds `Int | Bool`
  operands into `Int`; unary arithmetic / bitwise on bool yields
  `Int`. One-way only — `let b: bool = 1` still rejects. This was the
  canonical case `tyc::python_semantic_drift` was created to flag.
- **De Morgan narrowing on `not (A or B)`.** `collect_narrowings_inner`
  grows an `Expr::BoolOp` arm: in `if A and B:` both operands narrow
  in the true branch; in `if not (A or B): return` both operands
  narrow in the post-`if` branch (the early-exit refinement). The two
  ambiguous shapes (a bare `or`, `not (and)`) stay conservative since
  no single name can be safely refined. Closes the long-standing gap
  where `if not (x is None or y is None): use(x, y)` still rejected
  the `Optional` unwrap.

### Fixed — type checker

- **Fixed-arity tuple covariance on every slot.**
  `generic_param_variance` only declared `("tuple", 0) => Covariant`,
  so `tuple[int, int]` widened slot 0 to float but rejected slot 1.
  Promoted `tuple` / `Tuple` to a head-level early return so every
  slot is uniformly covariant; soundness preserved because tuples
  are immutable (same reason `tuple[T, ...]` was covariant already).
  Mutable-container invariance for `list` / `dict` / `set` is
  intentionally unchanged — the static rule guards the mutation
  hazard CPython's duck-typing can't see.
- **Subclass constructors accept inherited fields.**
  `class Dog(Animal):` previously rejected `Dog(name="Rex",
  breed="Husky")` with `tyc::unknown_kwarg 'name'` because the arity
  check only saw `Dog`'s direct field surface. Adds `bases:
  Vec<String>` on `InterfaceShape` and an `effective_class_shape()`
  helper that walks the inheritance chain (parent fields first, child
  overrides on collision, cycle guard); used at the constructor call
  site for both the kwarg validator and `class_constructor_arity`.
  Dotted base classes (`class Sub(pkg.Base):`) are recorded too —
  `effective_class_shape` silently skips bases with no shape entry,
  so foreign / unresolved bases are harmless.
- **Dotted-attribute annotations resolve to the foreign class shape.**
  `let c: foo.ApiClient = ...` used to land as `Type::Unknown`,
  silently dropping every downstream method-arity / kwarg check on
  `c`. New `Expr::Attribute` arm in `type_from_annotation` produces
  `Class("{module}.{attr}")` matching the call-site convention; carves
  out the permissive `typing.<X>` surface (`Any`, `Self`, `List`,
  `Dict`, `Tuple`, `Set`, `FrozenSet`, `Sequence`, `Iterable`,
  `Iterator`, `Mapping`, `Callable`, `Optional`, `Union`, `Final`,
  `ClassVar`, `Annotated`, `Literal`, `Type`, `TypeVar`) so a
  `-> typing.Self: return self` fluent-builder doesn't trip
  `type_mismatch`. Multi-segment paths (`a.b.Cls`) still fall back to
  `Unknown` pending full module-registry resolution.
- **`import foo as f; let c: f.Cls` canonicalises against `foo.Cls`.**
  The dotted-annotation arm produced `Class("f.Cls")` while the call
  site `f.Cls(...)` produced `Class("foo.Cls")` because the env
  resolves `f` to `Type::Module("foo")`, causing a spurious
  `type_mismatch` on every aliased-import handoff. Added
  `Checker::canonicalize_module_aliases` (walks `Class` / `Union` /
  `Generic` / `Function` recursively) as a second-chance comparison
  inside `is_assignable`.
- **`tyc::missing_field_init` catches container-literal + alias
  escapes.** A partially-initialised instance flowing into the RHS of
  an annotated assignment used to slip past the audit silently
  because the only tracked escape sites were `return` and call
  arguments. New hook runs `audit_check_escape` on the RHS of every
  `Stmt::AnnAssign` whose value mentions a tracked binding, covering
  `let configs: list[Config] = [c]` (container-literal escape) and
  `let alias: Config = c` (outer-scope alias escape). Self-reference
  in container literals (`let xs: list[T] = [c, c]`) also fires
  correctly. Bare `Stmt::Assign` stays intra-function aliasing and
  is intentionally not audited.
- **`BinOp` no longer over-promotes to float when one operand is a
  foreign class.** The previous `(Float, _) | (_, Float) => Float`
  arm caught `(Float, Class("torch.Tensor"))` and rejected
  `let y: torch.Tensor = 4.0 * x.sum()` despite the runtime
  `__rmul__` returning a Tensor. Now only fires when both sides are
  numeric primitives; otherwise falls through to `Unknown` so the
  surrounding annotation drives. Surfaced organically by the new
  corpus sweep against `examples/33-pytorch-tensors/`.

### Fixed — emit / VM correctness

- **Triple-quoted strings round-trip as triple-quoted.**
  `Expr::StringLiteral` in `tyc-emit` now emits triple quotes when
  the source used triple quotes or when the literal contains a real
  newline, instead of `\n`-escaping multi-line docstrings into
  single-line blobs that every formatter then fights with on save.
  New `escape_triple_quoted_string` helper breaks any internal
  3-quote run so the literal can't close prematurely. Skipped inside
  f-string interpolation (outer is `Some`) since nested triple quotes
  are almost always wrong.

### Internal / chore

- **Corpus round-trip CI sweep (`tests/pipeline.rs`).**
  `corpus_examples_all_check_clean` walks every `.ty` under
  `examples/` (excluding intentional-failure probes in
  `examples/testing/`) and asserts `tyc check` exits 0 on each. Any
  change to the type checker that breaks a previously-working example
  is caught in CI rather than discovered in a manual sweep. Closes
  the long-standing roadmap concrete-next-step #1.
- **Vendored Ruff insta tests re-enabled.** Flipped `[lib] test =
  true` on `ruff_python_parser`, `ruff_python_ast`, `ruff_text_size`;
  brings 266 upstream tests back online (217 + 49 + 0) with zero
  regressions against the Typhon `Mutability` extension. Closes the
  second of the two `tyc/vendor/README.md` deferred follow-ups.
- **`stress/round-2026-05-23-drift/` — three audit rounds.**
  Round 1: 15 probes targeting numeric / container variance under
  `int → float` widening. Round 2: inheritance + dotted-attr
  limitations. Round 3 (`45-probes-round-2-and-3.ty`): protocols,
  decorators, async, generics, exception flow, slicing, star-args,
  optional chaining — 30 probes, all green against the fixed
  checker. Documented per-case in
  `docs/diagnostics/python_semantic_drift.md`.

### Docs

- **Guide 10 — v0.3.0 language additions backfill.** Guide jumped
  straight from `.dty` stubs to "Putting it together" with no
  coverage of `newtype`, `freeze let`, or `pub`. Three new sections
  with emitted-Python comparisons and a `comptime` vs `freeze let`
  decision table. Salvaged from closed PR #102.
- **Roadmap reconciled with shipped reality.** Concrete-next-steps
  list updated: corpus round-trip sweep marked landed, loop
  parallelisation marked shipped (`tyc-analyse/src/parallel.rs`,
  opt-in via `[strictness] auto-parallel`), Salsa boundary entry
  notes that `preprocessed_text` / `resolved_module` /
  `check_diagnostics` / `module_shapes_query` are all
  `#[salsa::tracked]` (`check_file_with_imports` remains the
  multi-module bypass), `python_semantic_drift` "closed" set
  expanded with this release's work, native debugger remains
  unchanged-open.
- **`docs/diagnostics/arg_count.md` — inheritance section.**
  Rewrites the Limitations section to reflect the subclass-
  constructor fix; adds an Inheritance section recording the new
  walks-the-chain behaviour.
- **`docs/diagnostics/missing_field_init.md` — escapes-covered
  section.** Replaces the open-ended Limitations note with an
  explicit "Escapes covered" enumeration (return, call arg, container
  literal via annotated `let`, outer-scope alias via annotated
  `let`); intra-procedural + subclass caveats carried forward.
- **`examples/README.md`** gains a Language-additions row;
  `examples/48-...` comment corrected to reference
  `tyc::newtype_violation` (the dedicated diagnostic with wrap-fix
  help) instead of the generic `tyc::type_mismatch`.

## 0.3.1

A second stress campaign (`stress/round-2026-05-22/`, 93 fresh `.ty`
programs across nine domain buckets) filed thirteen new findings
(N1–N13, three of them silent-wrong-output CRITICAL). Every one of
them is closed in this release. Alongside that sweep: a new
`tyc::duplicate_method` diagnostic, typed tuple-unpacking on `let`,
a `tyc migrate` rewrite for the pre-PEP-695 `Generic[T]` idiom, a
`tyc run` correctness fix that gates VM execution behind a real
static check, and an LSP / `tyc check` polish pass.

This is a correctness-focused point release. No new headline language
features; the three CRITICAL fixes (N5, N6 emit parens, N9 VM `match`
scope) are the reason every adopter on 0.3.0 should upgrade.

### Added — language

- **Typed tuple-unpacking `let` (N4).**
  `let (a: int, b: str) = func(x, y)` now parses and desugars into a
  hidden `__typhon_unpack_N__` temp plus per-element `let` lines
  carrying the user-supplied annotations. Compound annotations
  (`list[int]`, `dict[str, int]`, `tuple[float, ...]`) survive the
  top-level-comma split, mixed captures (`let (a: int, b) = pair()`)
  emit the un-annotated leg with no type so inference fills it in,
  and the existing un-annotated `let (a, b) = pair()` form flows
  through unchanged.

### Added — diagnostics

- **`tyc::duplicate_method` (N8).** Two `impl Foo:` / `extend Foo:`
  blocks both defining `def get(self) -> …` used to merge silently,
  with Python keeping whichever definition desugar visited last. The
  new diagnostic anchors the second `def`, suggests rename / delete /
  merge, and runs after the type checker's class-shape merge so it
  doesn't double-report against `tyc::impl_unknown_class` for
  spurious `__typhon_impl_X` pseudo-classes.

### Fixed — emit / VM correctness (the three silent-wrong-output
finds)

- **`UnaryOp` paren wrap (N5, N6).** `not (a or b)` and
  `not (x if c else y)` no longer round-trip to the
  De-Morgan-violating `not a or b` / `not x if c else y` shapes.
  The `Expr::UnaryOp` printer arm now consults `expr_precedence` on
  the operand and wraps when the operand is lower-precedence
  (`Not` = 5, `Or` = 3, `And` = 4, `If` = 2, etc.). Surfaced
  organically against `05-agents/01-react-agent.ty` — a calculator-
  tool guard `not (ch.isdigit() or ch in " +-*/()")` was being
  emitted wrong.
- **VM `match` arm writes propagate to the outer scope (N9).** The
  tree-walking VM in `tyc-vm` ran each `case` arm in a fresh
  environment frame and discarded writes when the arm exited. Any
  `match` + accumulator pattern — every Result walker, every sealed-
  union aggregator, every state machine — saw `total = 0` from the
  VM and `total = 42` from `tyc run --compile`. The arm body now
  executes against the parent env, and pattern captures (`case
  Ok(v):` introducing `v`) are lifted into the parent env once the
  pattern + guard accept — matching CPython's `match` semantics,
  where bindings escape the arm. A failed pattern still discards
  any tentative captures so a partial bind on a non-matching arm
  never leaks.

### Fixed — `tyc run` / `tyc migrate`

- **`tyc run` gates the VM behind a static check (N10).** Previously
  the VM would happily evaluate a program with an unresolved name and
  crash with a Python-style `NameError`, hiding what would have been
  a clean `tyc::unknown_name` diagnostic. `tyc run` now runs
  `tyc check` first; only programs that type-check clean reach the
  VM.
- **`tyc migrate` rewrites `Generic[T]` → PEP 695 (N11).**
  Pre-3.12 generic idiom (`from typing import TypeVar, Generic`,
  `T = TypeVar("T")`, `class Box(Generic[T]):`) used to land in the
  output `.ty` unchanged, then trip
  `tyc::typevar_import_rejected` and the `Generic[T]` rejection on
  the very next `tyc build`. The rewriter now drops module-level
  `T = TypeVar(...)` declarations (including bounded /
  `constraints=` forms), rewrites `class X(Generic[T]):` into
  `class X[T]:` (multi-parameter, mixed-base, and qualified
  `typing.Generic` forms covered), and elides the now-dead `TypeVar`
  / `Generic` imports.

### Fixed — type-checker / desugar / emit

- **`freeze let` multi-line RHS (N1).** Multi-line dict and list
  literals on a `freeze let` no longer leak out of the synthesised
  `__typhon_freeze__(...)` call; the wrap now happens at the AST
  level, not as a text-level fix-up on the binding's first line.
- **`?` inside a comprehension fires a targeted diagnostic (N2).**
  `[parse(s)? for s in items]` used to silently hoist past the
  `for`-binding into a top-level `try`/early-return — semantics that
  no user wanted. The pre-pass now rejects `?` inside any kind of
  comprehension with a dedicated message ("`?` cannot be lifted out
  of a comprehension — rebind the result and unwrap it"); the
  outside-comprehension inline-`?` work (O17) keeps working.
- **Exhaustiveness on `match self.<field>:` (N13).** The
  exhaustiveness pass only inspected `Expr::Name` subjects when
  resolving the static type of a `match` subject. A class with a
  sealed-union field doing `match self.state:` therefore landed in
  the "subject type unknown" branch and the missing-return analysis
  treated the whole match as a potential fall-through (false-
  positive `tyc::missing_return` over a total match). The subject-
  type resolver now delegates `Expr::Attribute` to
  `infer_expr_readonly`. Non-exhaustive variants still surface both
  `tyc::non_exhaustive_match` and `tyc::missing_return`, as before.
- **`tyc::newtype_violation` covers boundary mismatches (N7).** A
  bare `int` flowing into a `UserId`-typed parameter used to fire
  `tyc::type_mismatch` with help text saying "expected UserId, found
  int" — the wrong-direction advice for a nominal alias, since the
  fix is `UserId(x)`, not "annotate as int." The newtype boundary is
  now routed through `tyc::newtype_violation` with help text that
  names the constructor call.
- **`from __future__ import annotations` not duplicated (N12).** A
  user who hand-wrote `from __future__ import annotations` at the
  top of a `.ty` file used to get a second one inserted by the emit
  pass, which Python's compiler then warned about and tools like
  `ruff format` would re-collapse on every save.
- **Comptime `str.join(...)` (N3).** Added to the comptime sandbox
  alongside the existing `str` / `int` / `float` / arithmetic /
  ternary / `env` surface. `comptime let URL: str = "/".join([host,
  path])` now evaluates at build time.

### Performance / tooling

- **Batched venv introspection + shared preprocess across check
  passes.** The third-party signature recovery introduced in 0.2.2
  used to fire one Python subprocess per module per `tyc check`
  invocation. The new batch path collects every public class /
  function across every imported module in one round-trip, and the
  preprocess output (rewrites for `let`, `pub`, `freeze`, …) is now
  shared across the resolver, the type-checker, the analyser, and
  the desugar pass via a Salsa-tracked query — a meaningful drop in
  end-to-end check time on every project that touches more than a
  couple of dependencies.

### LSP / `tyc check`

- **Semantic-token kinds for attribute access.** Bare-import access
  on a third-party class — `nn.Module`, `pd.DataFrame`,
  `torch.optim.Adam` — now paints as a class instead of falling
  through to the generic `property` / `method` token. The LSP
  introspects the receiver module's `dir(...)` once via the existing
  venv-signature cache and consults a `(receiver, attr) → kind` map
  before emitting each `Attribute` token.
- **Grouped `tyc check` diagnostics.** Errors are now grouped by
  source file (`-- errors in ./src/a.ty --`) instead of interleaved
  by analysis phase, with a per-code summary tally at the bottom
  (`1 error(s): tyc::arg_count`, `2 error(s): tyc::type_mismatch`)
  and an `tyc explain <code>` suggestion. CI logs cluster related
  errors instead of scattering them across files.
- **Surface introspection failure reasons in hover.** Hovering a
  third-party import whose subprocess introspection failed used to
  silently fall through to "no docs available." Hover now renders
  the actual reason (`NoPython`, `ImportFailed`, `Timeout`,
  `SpawnFailed`) and the recovery hint (`Install it with
  \`tyc add torch\`.`).
- **Per-module introspection timeout 3 s → 10 s** so heavy ML
  packages (`torch.nn` triggers C-extension init in the multi-
  hundred-ms range) complete on cold first-import instead of
  timing out.
- **Prewarm the actual dotted module path.** `import torch.nn as nn`
  now warms `torch.nn`, not just `torch`, so the first `nn.<dot>`
  doesn't block on the subprocess.
- **VS Code grammar.** The bundled TextMate grammar gains the v0.3.0
  keywords (`freeze`, `newtype`, `pub`, `frozen`, `plain`, `class!`),
  highlights `newtype X = Base` declarations alongside the existing
  type-alias rule, and accepts stacked modifier chains (`pub freeze
  let`, `pub comptime let`).

### Stress round 2026-05-22 (recorded in `docs/findings.md`)

- 93 fresh `.ty` programs across `01-language-edge` (incl. paren-
  precedence + freeze + match), `02-io`, `03-ml-numpy`,
  `04-ai-llm`, `05-agents`, `06-api`, `07-sdk`, `08-meta-stress`,
  `09-error-quality`.
- 13 findings filed (3 CRITICAL silent-wrong-output, 5 HIGH,
  2 MEDIUM, 3 LOW). All closed in this revision with a regression
  test plus the original stress probe retained as a forward-looking
  guard.

## 0.3.0

Six new language features for everyday Python annoyances, a coordinated
sweep that closes every remaining open finding from the May 2026 stress
campaigns (O2–O29), and cross-platform install support: pre-built `tyc`
binaries for **Linux (x86_64 + aarch64)** and **Windows (x86_64)** join
the existing **macOS (Apple Silicon + Intel)** matrix.

### Added — language

- **`newtype Name = Base` — nominal aliases over base types.**
  TypeScript-style nominal aliasing that keeps same-shaped primitives
  (`UserId` vs `PostId`, `USD` vs `EUR`, internal vs external IDs)
  from being silently swapped. Asymmetric by design: a `UserId` flows
  into an `int`-typed slot (the runtime value *is* an `int`), but a
  bare `int` requires an explicit `UserId(x)` constructor call to
  satisfy a `UserId`-typed target. Compiles to a zero-cost
  `typing.NewType` call. New `tyc::newtype_violation` diagnostic.
- **`freeze let X = expr` — deep-immutable bindings.** Closes the gap
  left by `let`, which only locks the binding name and not the
  underlying value. `freeze let` wraps the RHS in
  `__typhon_freeze__(...)` from `typhon_runtime.freeze`, which
  recursively converts `list → tuple`, `dict → MappingProxyType`,
  `set → frozenset`, and descends into nested values. Anything without
  a clean immutable equivalent (file handles, sockets, generators,
  non-frozen dataclasses) raises `TypeError` at startup rather than
  via a confusing downstream mutation.
- **`pub` modifier for module-level visibility.** When a module
  declares at least one `pub` name, desugar synthesises a top-of-file
  `__all__ = [...]` list so `from foo import *`, Sphinx autoapi, IDE
  re-export filters, and the checker's re-export inference all see
  the public surface — no hand-maintained `__all__` lists required.
  Composes with the existing keyword stack (`pub let`,
  `pub frozen class`, `pub model`).

### Added — diagnostics

- **`tyc::blocking_in_async`.** Catches direct calls to known-blocking
  stdlib functions (`time.sleep`, `requests.get`, `socket.recv`,
  `subprocess.run`, `input`, `urllib.request.urlopen`, …) inside an
  `async def` body. A blocking call halts the entire event loop until
  it returns; the diagnostic suggests `asyncio.to_thread(...)` /
  `loop.run_in_executor(...)`. Suppressed inside `unsafe:` regions.
- **`tyc::resource_not_managed`.** Flags bare assignments of
  context-manager-returning calls (`open`, `socket.socket`,
  `sqlite3.connect`, `tempfile.*`) that aren't wrapped in a `with`
  statement, where deterministic cleanup matters and the runtime
  would otherwise leave teardown to the garbage collector. Severity
  defaults to `warn`; controlled by `[strictness] resource-not-managed`.
- **`tyc::div_by_zero_literal`.** Catches `x / 0`, `x // 0`, and
  `x % 0` at compile time when the divisor is a literal — `0`, `0.0`,
  `-0`, `-0.0`, or any unary-negated form. Pure constant-fold lint
  with zero false positives. Flow-sensitive analysis (`if d != 0:`
  guards on runtime values) is deliberately out of scope.
- **`tyc::unsafe_value_leak`** (O14). A `return x` outside the
  `unsafe:` block where `x` was declared, against a function with a
  concrete annotated return type, now fires a dedicated diagnostic
  with help text pointing at both workaround forms (`let x: T = …`
  inside the block, or `let typed: T = x` outside).
- **`tyc::pattern_shadows_outer`** (O10). `case Wrap(value):` against
  an outer `let value` now fires a dedicated diagnostic instead of
  the misleading `tyc::immutable_assign`. Suggests renaming the
  capture — the right advice for the Rust/OCaml/Scala intuition every
  newcomer brings to `match`.
- **`tyc::extend_builtin`** (O23). `extend list[int]:` (parametric
  target) now fires a dedicated diagnostic naming the parametric
  shape, rather than the confusing downstream
  `tyc::impl_unknown_class` cascade.

### Added — install + release

- **Linux pre-built binaries.** `tyc-<version>-x86_64-unknown-linux-gnu.tar.gz`
  and `tyc-<version>-aarch64-unknown-linux-gnu.tar.gz` ship on every
  release tag. The same `install.sh` now detects `Linux` from
  `uname -s` and resolves the matching tarball; the macOS path is
  unchanged (Gatekeeper quarantine xattr clearing is skipped on
  Linux). Built on `ubuntu-22.04` for broad glibc compatibility.
- **Windows pre-built binaries.** `tyc-<version>-x86_64-pc-windows-msvc.zip`
  ships on every release tag, alongside a new `install.ps1`
  PowerShell installer. The script downloads the zip, verifies its
  SHA-256, extracts to `%LOCALAPPDATA%\Programs\Typhon` by default,
  and adds the directory to the user-level `PATH` via
  `[Environment]::SetEnvironmentVariable`. Supports `--Version` and
  `--InstallDir` flags + `TYPHON_VERSION` / `TYPHON_INSTALL_DIR`
  env vars.
- **Release workflow.** `.github/workflows/release.yml` now runs a
  five-job matrix (macOS Apple Silicon + Intel, Linux x86_64 +
  aarch64, Windows x86_64) and uploads tarballs + the Windows zip +
  a combined `SHA256SUMS` file to the GitHub Release. Linux
  aarch64 cross-compiles from Ubuntu via the `aarch64-linux-gnu-gcc`
  toolchain.

### Findings sweep — every open finding closed (O2–O29)

This release coordinates the two final findings-closure branches
(`claude/findings-documentation-review-HhuVH` and
`claude/finish-open-findings-dmLv6`). Across nine stress campaigns
(May 17–21 2026, ~600 hand-written `.ty` programs, ~120 distinct
findings) the **Open** column on `docs/findings.md` is now empty
for the first time since campaign tracking began.

- **`tyc fmt`** (O12, B9) — five new PEP 8 rules wired into the
  in-process pass: space after `:`, spaces around `->`, space after
  `,`, single-space around binary `+` / `-` and top-level `=`, two
  blank lines before top-level `def` / `class` / `async def`. The
  O12 repro `def    f(  x:int,y:int)->int:` reformats end-to-end.
- **TypedDict-style dict literals** (O15). `let alice: User = {"id":
  1, "name": "Alice"}` against a registered class shape now matches
  keys against fields and flows each value under the declared field
  type before falling through to the ordinary `dict[K, V]` path.
- **Sized-style Protocols on built-ins** (O16). `list`, `dict`,
  `tuple`, `set`, `str`, `bytes`, `range`, `frozenset`, `bytearray`
  now satisfy a user-declared Protocol whose declared methods are
  all common dunders (`__len__`, `__iter__`, `__getitem__`,
  `__contains__`, `__eq__`, `__bool__`, …).
- **Inline `?`** (O17). `Ok(add(parse(s)?, parse(t)?))` compiles. A
  new `expand_inline_question_ops` pre-pass lifts every mid-line `)?`
  into a `__typhon_qi_N__` temp + propagation guard before the
  existing end-of-line pass runs.
- **`while` narrowing** (O2). `Stmt::While` now applies test-implied
  narrowings to the body via the same path the `if` checker uses;
  the linked-list iterator idiom
  `while cur is not None: total += cur.value; cur = cur.next`
  type-checks.
- **`tuple[T, ...]`** (O3) — resolves to an internal
  `tuple_variadic[T]` head that the unifier accepts against any
  fixed-length tuple literal whose elements are all assignable to
  `T`, including `()`.
- **Cyclic type aliases** (O4) — surface `tyc::cyclic_type_alias`
  once, then rewrite the alias body to `Any` so subsequent uses fall
  through silently instead of cascading into `type_mismatch` errors.
- **`class! Foo(Exception)`** (O5) — synthesises
  `def __init__(self, *args, **kwargs)` calling `super().__init__`
  when the body has no annotated fields, so `raise AppError("boom")`
  reaches the parent constructor.
- **Generator `return`** (O6) — `return` and `return value` inside a
  generator function body type-check against the declared
  `Iterator[T]` / `Generator[Y, S, R]` return type.
- **`tyc migrate` nullable forward-refs** (O21) — `Optional["Item"]`
  now becomes `"Item?"`, not the previously unparseable `"Item"?`.
- **`tyc migrate` Union → `T?`** (O22) — rewrites `Union[T, None]`
  (and `Union[None, T]`, and the `typing.Union[...]` qualified form)
  to `T?` and drops the import; PEP 604 pipe-union fallback for
  multi-arm unions so imports are never left dangling.
- **`tyc explain --list`** (O25) — now prints every diagnostic code
  the binary knows about; the "not yet implemented" message for
  unknown codes is gone.
- **VM `Result` repr** (O24) — `Value::ResultOk` / `Value::ResultErr`
  match CPython's dataclass default (`Ok(value=20)` /
  `Err(error='oops')`); single-quote preference matches Python's
  `repr`. `tyc run` and `tyc run --compile` now produce
  byte-identical stdout for Result-bearing programs.
- **REPL prompts when piped** (O26) — `tyc repl` checks
  `stdin.is_terminal()` and skips the `>>> ` / `... ` prompts when
  stdin is piped.
- **`tyc build --no-sync`** (O29) — new flag (and `TYC_NO_SYNC=1`
  env var) skips the `uv sync` step while still merging
  `pyproject.toml`. Stress harnesses and REPL-like iteration on
  tmp projects no longer pay the per-invocation reprovision cost.
- **`MatchSequence` bracket recovery** (O27) — `case [a, b]:`
  re-emits as `[a, b]` rather than the default `(a, b)` by peeking
  at the original `TextRange` to recover the bracket choice.
- **Pipe corner cases** (O28) — `5 |> (lambda x: x * 2)()` and
  `(1 |> add(2)) |> add(3)` both compile. New
  `expand_pipes_in_subexpressions` pre-pass recursively expands
  pipes inside every balanced `(...)` group before the line-level
  pass runs.

### Tests

- 4 new preprocess unit tests for `newtype` lowering (`Name =
  NewType(...)` round-trip, generic bases, fmt round-trip,
  indented-form rejection).
- 7 new unit tests for `tyc::div_by_zero_literal` covering `/`,
  `//`, `%`, float zero, negated zero, and `unsafe:` suppression.
- New regression tests for `tyc::blocking_in_async`,
  `tyc::resource_not_managed`, `tyc::unsafe_value_leak`,
  `tyc::pattern_shadows_outer`, and `tyc::extend_builtin`.
- `tyc fmt` regression suite covers each of the five new PEP 8
  rules end-to-end.
- All B9-bucket pre-existing-bug repros under
  `stress/round-2026-05-21/` build clean.

## 0.2.5

Second editor-UX pass on the 0.2.4 LSP work. Closes the three
remaining "doesn't match what VS Code does for Python" gaps in
the original report:

### Added

- **Module-path tokens in `from X.Y import Z`.** Dotted module
  paths in both `from` and bare `import` statements now emit a
  `namespace` token per segment, with `defaultLibrary` applied for
  stdlib roots. Previously the resolver only emitted a token for
  `Z` (the binding), leaving `X.Y` uncoloured — visually the most
  prominent part of every import line. Each segment is its own
  token so themes (and future "go to import" code actions) can
  target sub-segments.
- **Shape-aware kwarg colouring.** Call sites like
  `Agent(client=…, model=…)` now classify each kwarg name against
  the callee's real signature:
  - `property` (orange) when the kwarg names a declared parameter.
  - `parameter` (yellow) when the callee declares `**kwargs` and
    the kwarg isn't on the explicit list.
  - No token (white) when the kwarg is unrecognised and the callee
    has no catch-all — a visible cue that something's off without
    us claiming a hard diagnostic.

  The LSP pre-resolves callee signatures by batch-querying the
  introspection cache for every top-level imported class /
  function in the open buffer. Signature parsing is tolerant of
  nested-tuple defaults (`a=(1, 2)`), quoted commas
  (`sep=', '`), positional-only `/`, kw-only `*`, and the
  conventional `self` / `cls` first parameter.
- **Class docstring fallback to `__init__`.** When a class body
  carries no docstring (common pattern when the author wrote the
  documentation on `__init__` instead), hover now falls back to
  `__init__.__doc__` so the popover still shows parameter
  documentation. Without the fallback the hover for classes like
  `agent_framework.Agent` degraded to "📦 from …" with no Markdown
  body.
- **RST inline cleanup in hover docstrings.** Two of the most
  common Sphinx markup shapes are normalised to Markdown before
  rendering: ``\`\`code\`\``` (RST double-backtick) becomes
  ``\`code\``` (Markdown), and Sphinx role markup like
  `:class:\`Foo\``, `:func:\`bar\``, `:meth:\`baz\`` has the role
  stripped, leaving just the backticked symbol. Pylance does the
  full Sphinx → HTML render; this is the 80/20 that catches the
  shapes appearing in most third-party docstrings.
- **Signature truncation cap raised** from 400 → 1024 characters
  so Pydantic / SQLAlchemy / Django model constructors with 20+
  typed parameters render their full signature instead of being
  dropped entirely.

### Tests

- 10 new tests in `tyc-lsp::semantic`:
  - Module-path emission: `from foo.bar import Baz`,
    `from os.path import join` (stdlib modifier), dotted
    `import foo.bar.baz`.
  - Kwarg classification: real-param → `property`,
    `**kwargs` catch-all → `parameter`, unknown → no token.
  - Signature parser: basic param names, `**kwargs` detection,
    nested-default commas, `self` / `cls` skip.
- 2 new tests in `tyc-lsp` for `render_docstring`: RST
  double-backtick code, Sphinx role directive stripping.

## 0.2.4

Editor developer-UX pass: semantic-token colouring for the LSP and
structured docstring rendering in hover. The two together close the
gap users hit on their first day with Typhon — "which of these
imports is from my project vs the library?" and "what arguments
does this take and what do they mean?".

### Added

- **LSP semantic tokens.** `textDocument/semanticTokens/full` is now
  served. Walks the resolved module and parsed AST to emit a token
  stream tagged with LSP-standard types (`class`, `function`,
  `method`, `property`, `parameter`, `variable`, `namespace`) and
  modifiers (`declaration`, `defaultLibrary`). VS Code themes apply
  colours automatically — stdlib imports get the `defaultLibrary`
  shade (Python convention: muted blue), third-party / project
  imports get the usual class/function colours, and method calls
  (`agent.run()`) are coloured distinctly from property reads
  (`agent.name`). The legend is published in the server capabilities
  so the indices stay stable across releases.
- **Structured docstring sections in hover.** `render_docstring` now
  detects Google / NumPy / Sphinx-style sections (`Args:`,
  `Parameters\n----------`, `:param name:`) and re-renders them as
  Markdown headers + bullets. Parameter lines (`name: desc`,
  `name (type): desc`, `name : type\n    desc`) become
  `- **name** — desc` bullets, so the hover popover shows the
  parameter list with descriptions instead of a wall of indented
  text. Free prose, examples, and unrecognised sections are
  preserved verbatim.

### Tests

- 7 new tests in `tyc-lsp::semantic` covering stdlib /
  third-party / project token classification, declaration-modifier
  emission on class + function decl sites, method-vs-property
  detection in `Expr::Attribute`, and LSP delta-encoding
  correctness.
- 5 new tests in `tyc-lsp` for `render_docstring`: Google `Args:`,
  NumPy `Parameters\n----------`, Sphinx `:param X:`, recognised
  `Examples:` section, and the pass-through for unstructured
  prose. The existing PEP 257 indent-strip test is updated to
  assert on the new structured output.

## 0.2.3

UX polish on the arity diagnostic landed in 0.2.2 plus full hover
developer-UX for third-party imports in the LSP. Lands as one
release: the diagnostic naming and the hover preview are the same
"tell me what's wrong / tell me what this thing is" story.

### Added

- **`tyc::missing_argument` diagnostic.** When the checker can
  pinpoint *which* parameter wasn't supplied (constructor or
  free function), the new code fires instead of the count-based
  `tyc::arg_count` — so `Agent(name=..., tools=...)` now reads
  `missing required argument to 'Agent': 'client'` and a one-line
  `help: supply 'client' when calling 'Agent'`, instead of the
  misleading "expected 1, got 4". Multiple missing names render as
  ``` `a`, `b` ``` with the plural form. `tyc::arg_count` still
  fires for shape mismatches that can't be reduced to a missing-name
  list — `missing_required_fields` and `missing_required_params`
  detect positional+kwarg double-binding, too-many-positionals, and
  `*iter` / `**dict` unpacks and return empty so the count-based
  diagnostic stays the source of truth for those cases.
- **LSP hover docs for third-party imports.** Hovering an imported
  class or function in VS Code (or any LSP-aware editor) now shows
  the source module, the recovered signature in a fenced `python`
  code block, and the runtime docstring rendered as proper Markdown
  — pulled from the same venv-introspection cache that already
  powers completion. Project / stdlib symbols fall through to the
  existing kind-only hover.
- **Responsive prewarm on document open / change.**
  `check_and_publish` spawns a detached task that introspects every
  third-party import in the open buffer the moment the file is
  parsed, so the cache is hot by the time the user hovers. The
  first hover used to wait on the subprocess (30–100 ms typical);
  subsequent hovers hit the cache. The prewarm never blocks
  diagnostics publishing, and is debounced by document version so
  rapid typing doesn't queue redundant blocking-thread work.
- **Markdown rendering for hover.** Hover now publishes
  `MarkupContent { kind: Markdown, … }` instead of the older
  `MarkedString::String` shape so signatures appear in fenced
  code blocks and docstrings render as paragraphs.
- **Multi-line docstrings.** The Python introspection script now
  returns the full docstring (up to 4 KB) instead of the first
  line; the LSP strips PEP 257 indentation and trims surrounding
  blanks before rendering. Caps the visible body at 40 lines with
  an explicit truncation marker so module-level docstrings (numpy,
  pandas, sklearn) don't flood the popover.
- **Off-runtime introspection in hover.** The hover handler runs
  the cold-path `cache.members()` call through
  `tokio::task::spawn_blocking` so a worst-case 5-second timeout
  can never stall the async runtime. The prewarm makes cold hits
  rare; this is the safety net for the case where the prewarm
  hasn't completed yet.
- **Poisoned-mutex recovery on hover + prewarm.** Both paths now
  recover poisoned `IntrospectionCache` mutexes via
  `into_inner()`, matching the completion path. A prior panic no
  longer permanently disables hover / prewarm for the rest of the
  session.

### Tests

- The three `tyc-db` cross-module tests and the four `tyc-types`
  arity tests that asserted on "wrong number of arguments" now
  match the new wording (and assert on the specific missing name).
- 4 new regression tests in `tyc-types` covering the
  `missing_argument` guards: positional+kwarg double-binding (ctor
  + free fn), too-many-positionals (ctor + free fn with kw-only
  required).
- 5 new tests in `tyc-lsp` covering `render_docstring` (PEP 257
  strip, blank-line trim, 40-line cap, empty input) and `sig_tail`
  (prefix strip, unknown-shape pass-through).
- One `tyc-types` arity test on `def add(a, b)` accepts either
  wording to stay forward-compatible with future diagnostic
  refinements.

## 0.2.2

Third-party signature recovery via venv introspection. The flagship
gap this release closes: a project that imports a class from an
unstubbed PyPI package (`from agent_framework import Agent`) and
calls it missing a required argument (`Agent(name="x", tools=[])`
without the required `client` kwarg) passed `tyc check` and
`tyc build` clean in 0.2.1, then crashed at runtime with
`TypeError: Agent.__init__() missing 1 required positional
argument: 'client'`. The checker had no signature for `Agent`
because no `.dty` stub was authored, so the callable degraded to
`Type::Unknown` and the arity check at
`tyc/crates/tyc-types/src/lib.rs:6061` (which only fires for
project-local classes) was skipped.

0.2.2 closes the loop by shelling to the project's
`.venv/bin/python` (or a fallback `python3` on PATH), asking
`inspect.signature` for the real parameter list of every public
class and free function in each declared dependency, and folding
the result into the same `ModuleShapes` registry that
`tyc-db::build_external_shapes` already consumes. No changes to the
checker itself — once the shape is in the registry, the existing
cross-module constructor / function arity path fires identically
to in-project calls.

### Added

- **`tyc/src/venv_signatures.rs`: venv-driven signature
  introspection for the type checker.** Walks every `.ty` file's
  `import` / `from ... import ...` / `lazy import` statements to
  collect dotted module names, then runs an embedded Python helper
  in the project venv that emits structured per-parameter info
  (name, kind, has-default) for every public class and free
  function. The Rust side converts that into an `InterfaceShape`
  (for classes — each `__init__` param becomes a field, with
  defaulted params populating `field_defaults`) or an `ArityInfo`
  (for free functions — kw-only / positional / `*args` / `**kwargs`
  preserved), and merges the result into the project shape
  registry. `tyc check` and `tyc build` consume the enriched
  registry; no changes were needed in `tyc-types` or `tyc-db`.

### Safety rails

- Only modules whose top-level package is listed in
  `[dependencies]` / `[dev-dependencies]` (or maps to a declared
  distribution via `.dist-info/top_level.txt`) are introspected.
  Stdlib (`os`, `json`, `collections`) and project modules stay on
  their existing resolution paths — no Python subprocesses for
  ordinary stdlib usage.
- Classes whose `__init__` declares `*args` or `**kwargs` are
  deliberately skipped. False positives on permissive Python APIs
  (every extra kwarg firing `tyc::unknown_kwarg`) would be worse
  than the existing miss.
- All failures (no venv, no Python on PATH, import-time exception,
  5-second introspection timeout) silently no-op. Worst case is the
  prior 0.2.1 behaviour — the runtime catches what the checker
  couldn't.
- Subprocess `current_dir` is pinned to the project root so the
  same `tyc check` from any subdirectory produces the same shape
  registry. Same reproducibility contract that already applies to
  `tyc::unknown_module`.
- The introspection result is cached per `VenvSignatures` instance
  keyed by dotted module name; one subprocess per module per
  `tyc check` invocation.

### Tests

- 10 new unit tests in `tyc::venv_signatures` covering the
  shape-conversion logic (required kw-only params, `**kwargs`
  bail-out, `*args` bail-out, positional+kw-only mix, `*args` /
  `**kwargs` on free functions, dotted-name validation, allow-list
  gating, and import-statement extraction across every Typhon /
  Python form).
- One new integration test in `tyc::commands::check::tests`
  (`check_introspects_third_party_class_constructor_arity`) that
  builds a fake third-party package, runs the introspection
  helper, and asserts the recovered shape models the original bug.
  Skips silently when no Python 3 is on PATH so CI runners without
  Python don't fail the suite.

### Caveats

- An author who genuinely needs the missing-arity check on a
  class with `**kwargs` should write a `.dty` stub — the stub
  declares the real surface and the arity check fires normally.
  See `docs/guides/08-…-stubs` (TBD).
- Python imports can have side effects. Modules that touch the
  network or sleep at import time would now do so during
  `tyc check`. The 5-second timeout caps the cost; users on
  pathological packages can declare the dep out of
  `[dependencies]` to skip introspection.

## 0.2.1

Correctness fixes surfaced by the
`stress/round-2026-05-20-exploration` corpus (~80 fresh `.ty`
programs across syntax edges, I/O, ML/NumPy, AI/LLM clients, agents,
HTTP servers, and SDK clients).  Documented per finding under
`stress/round-2026-05-20-exploration/FINDINGS.md`.

The flagship fixes in this point release are two **silent
wrong-output / wrong-rejection** bugs that the prior round's stress
corpus shook out:

- A pathological hole in the emitter precedence table that let
  `(a + b).upper()` round-trip as `a + b.upper()` — same semantics
  problem as the walrus bug fixed in 0.2.0, but with attribute /
  call / subscript / boolean-op as the outer context instead of an
  arithmetic BinOp.  Surfaced live in `pathlib.Path / "x"` chains and
  a NumPy least-squares reduction (`(dx * dy).sum() / (dx * dx).sum()`).
- The type checker refused to accept `None` or `T?` values against an
  `impl X: def f(self, p: T?)` parameter — the method's per-param
  types were never recorded and fell back to a `Vec![Type::Unknown; arity]`
  shape, which collided with the call-site
  nullable-into-non-nullable guard.

### Fixed

- **`tyc-emit`: parentheses preserved around non-atomic expressions
  used as the receiver of postfix ops.**  `Attribute`, `Call`,
  `Subscript`, and `BoolOp` emit arms now route the inner expression
  through a `needs_paren_for_postfix` guard.  Adds wrapping around
  `BinOp` / `BoolOp` / `UnaryOp` / `Lambda` / `IfExp` / `Compare` /
  `Named` (walrus) / `Await` / `Yield` / `YieldFrom` / `Starred` /
  `Generator` whenever they appear in those four contexts.  Also
  parenthesises a lower-precedence `BoolOp::Or` child inside a
  `BoolOp::And` parent.  (FINDINGS E1 — silent precedence bug.)
- **`tyc-types`: `MethodSig` now records per-parameter types.**
  `collect_class_shape` populates `MethodSig::param_types` from the
  declared annotations (stripping the implicit `self` / `cls` slot
  to mirror `arity_info`).  The call-site resolution path for
  `instance.method(...)` reads these into the `Type::Function::params`
  it returns, so `instance.method(value)` is now type-checked against
  the real declared parameter types instead of `Vec![Type::Unknown; arity]`.
  Fixes `impl X: def f(self, p: T?)` rejecting `None` and `T?`
  arguments.  (FINDINGS E2 — impl `T?` params.)
- **`tyc-types`: `self` inside an `impl` block carries the user-facing
  class name, not the desugarer's `__typhon_impl_X` pseudo-class.**
  The pseudo-class is an intermediate shape that the merge pass folds
  into the real class; previously the checker walked the methods
  first and `return self` against `-> X` failed with
  `expected X, found __typhon_impl_X`.  Stripping the prefix at
  `self`-receiver binding time gives the user the diagnostic surface
  they expect.  (FINDINGS E3 — broke `__enter__` / context-manager
  patterns.)
- **`tyc-types`: call-site `func_type` is unwrapped through type
  aliases.**  Calling through a transparent
  `type Handler = Callable[[Req], Resp]` alias now resolves to the
  underlying `Type::Function`, so the call's return type is `Resp`
  rather than `Handler`.  Middleware / decorator / handler-pipeline
  patterns work without inlining the `Callable[...]`.  (FINDINGS E4.)
- **`tyc-types`: user-defined dunders take precedence over the
  numeric-coercion table in `BinOp` inference.**  `Vec2(...) * 5.0`
  for `impl Vec2: def __mul__(self, scalar: float) -> Vec2` now
  resolves to `Vec2`, not `Float`.  Also reaches for the reflected
  dunder (`__radd__` / `__rmul__` / …) when the LHS is a primitive
  and the RHS is a user class.  (FINDINGS E5.)
- **`tyc-types`: exhaustiveness recognises positional class
  patterns.**  `case Leaf(value):` / `case Branch(left, right):`
  (positional captures of every declared field) now count as total
  matches for the named variant, so `missing_return` no longer fires
  on legitimately-total `match` statements over recursive sealed
  unions.  (FINDINGS E6 / R3.12 follow-up.)
- **`tyc-analyse`: comptime f-strings.**  `comptime let TITLE: str = f"{APP} v{MAJOR}.{MINOR}"`
  now evaluates at build time as long as every interpolation is
  itself a comptime constant.  Format specs and conversion flags
  (`!r` / `!s` / `!a` / `:>5`) remain unsupported and surface as
  explicit comptime errors instead of being silently dropped.
  (FINDINGS E7.)
- **`tyc-types`: calling a `Type::Class(...)` whose shape we don't
  have degrades to `Unknown` instead of treating it as a
  constructor.**  Catches the `self.linear(x)` shape where `linear`
  is an instance field typed as a foreign class (e.g. `torch.nn.Linear`) —
  the call is most likely invoking `__call__`, not constructing a
  fresh instance.  Pre-fix the imported class's "constructor" result
  would leak into `mut`-bound rebinds; post-fix the result is
  `Unknown` and the surrounding annotation drives assignability.
  (Knock-on fix surfaced by E3.)

### Tests

- 7 new regression tests in `tyc-types` covering E2, E3, E4, E5,
  E6 (positional / shorter-positional class patterns), and the
  dunder-rejects-arg-type-mismatch case added during PR-#87 review.
- 3 new regression tests in `tyc-analyse` covering E7 (the happy
  path, the format-spec rejection, and the
  list/tuple/dict-interpolation rejection added in PR-#87 review).
- 6 new emit round-trip tests in `tyc-emit` covering the
  paren-stripping shapes (`(BinOp).attr`, `(BinOp)[idx]`,
  `(ternary).attr`, `(lambda)(arg)`, `(Or).And`,
  `(path / "x").method(...)`).

### Documented in `stress/round-2026-05-20-exploration/`

A new stress round runs the full eight-domain corpus against this
release.  The remaining open issues (E8 class-const slot
descriptors, E9 `?` in subexpressions, E10 `for`-target rebinding,
E11 per-build venv DX, and the typed-SDK `dict[str, object]`
variance issue) are catalogued in
`stress/round-2026-05-20-exploration/FINDINGS.md` for the next
round.

## 0.2.0

Constructor / method arity safety. The flagship bug this release
catches: a class declared with `class ApiClient: api_key: str` that
the user instantiates as `ApiClient(base_url="…")` — passing `tyc
check` and `tyc build` in 0.1.6, crashing at runtime with `TypeError:
missing 1 required positional argument`. v0.2.0 surfaces the same
bug at check time, before the build ever runs.

### Review-driven fixes (post initial v0.2.0 RC)

- Bare-import dotted access now uses qualified `module.class` names
  internally, so two imports exporting the same class don't collide
  in the lookup table. Diagnostics also surface the qualified name
  (`clients.ApiClient`) for disambiguation.
- Same fix applied to imported free functions.
- `model X:` declarations correctly require every non-defaulted
  field at construction even when defaults appear earlier in the
  body — `ArityInfo` now carries a per-param `required_positional`
  flag rather than relying on the "all required come first" Python
  convention.
- `impl X:` / `extend X:` blocks contributing fields with defaults
  now merge `field_defaults` correctly; previously they were treated
  as required at construction.
- `[project] src = "app/src"` (nested src) now derives dotted names
  correctly — the basename of the configured src dir is what
  `path_to_dotted` actually needs.
- The post-construction audit no longer fires on `return c.field` or
  `f(c.field)` (receiver-of-attribute-access isn't an instance
  escape) and no longer emits duplicate diagnostics for repeated
  names like `return c if cond else c`.
- `setattr(obj=c, name=…, value=…)` (kwarg form) now drops audit
  tracking like the positional form does.
- `infer_expr_readonly` handles `Expr::Call` so chained calls like
  `clients.ApiClient(…).url(…)` resolve the receiver type for
  method arity checks.
- The project shape registry is now `Arc<HashMap<…>>` end-to-end so
  per-file `ExternalShapes` snapshots are O(1) refcount bumps
  instead of O(modules) deep clones.

### Added

- **`tyc::arg_count` now fires on class constructors.** The
  auto-generated `__init__` of `class` and `model` declarations is
  arity-checked at every call site. Fields with no `= default` are
  required; fields with a default are optional. `T?` without an
  explicit `= None` is still required (Typhon doesn't auto-inject the
  default), matching the emitted dataclass's runtime semantics.
- **`tyc::arg_count` now fires on `impl` methods.** Method signatures
  carry full `ArityInfo` (param names, defaults, `*args`/`**kwargs`)
  instead of a single arity count. `user.greet()` is flagged when
  `greet` declares a required `prefix: str` parameter.
- **Cross-module arity checks.** `from foo import ApiClient`
  followed by `ApiClient(…)` arity-checks against `foo`'s exported
  shape. `import foo as f` followed by `f.ApiClient(…)` does too via
  the new `Type::Module(name)` modelling. Both `.ty` source and
  `.dty` stubs flow through a project-wide shape registry built once
  per invocation. Works in `tyc check`, `tyc build`, and the LSP.
- **Salsa-cached shape extraction.** A new
  `tyc_db::module_shapes_query(file)` salsa-tracked query caches
  per-file shape extraction. The LSP backend keeps a per-project
  `HashMap<dotted_name, SourceFile>` so handles survive across
  keystrokes; a keystroke in one file only re-runs extraction for
  that file.
- **`tyc::missing_field_init` post-construction audit.** Catches the
  `X.__new__(X)` / `object.__new__(X)` bypass-construction pattern:
  if the instance escapes the function (return / call argument)
  without every required field assigned, the audit fires. Dropped
  conservatively on `setattr`, on `obj.method(…)` calls, and inside
  `unsafe:` regions.
- Public API in `tyc-types`: `InterfaceShape`, `MethodSig`,
  `ArityInfo`, `ModuleShapes`, `ExternalShapes`, plus
  `extract_module_shapes` and `check_module_with_imports`. Downstream
  tools building on the type-check pipeline now have a stable
  surface for cross-module checks.

### Changed

- `Type::Module(String)` joins the type enum, exposed for the bare-
  import attribute-access path.
- `tyc-db` re-exports `ModuleShapes` so CLI and LSP callers don't need
  to depend on `tyc-types` directly.
- The check pipeline's `Type::Function` arm now consults
  attribute-callee arity info before falling through to the
  permissive shape, closing the long-standing method-arity gap.

### Limitations

- Dotted-attribute annotations (`let c: f.Cls = …`) don't yet resolve
  to the foreign class shape. The constructor call itself
  arity-checks, but the binding lands as `Type::Unknown`. Workaround:
  use `from foo import Cls` or drop the annotation.
- The post-construction audit only flags return statements and
  function-call arguments as escapes; container-literal storage
  (`return [c]`) and outer-scope assignment aren't tracked.
- The audit is intra-procedural. A method that genuinely initialises
  fields suppresses the diagnostic (correctly); a method that
  doesn't will also suppress it (false negative).
- The audit doesn't track subclass field requirements separately.

### Migration notes from 0.1.6

The new arity checks are strict by default and may surface
pre-existing latent bugs in your codebase — that's the point. Two
patterns commonly need attention:

1. `Foo()` calls where `Foo` has required fields. Either add the
   missing arguments or give the fields defaults (`field: str = ""`).
2. Method calls that previously passed under the permissive shape.
   If a method signature changes (e.g. you add a parameter), every
   call site is now flagged immediately.

There are no behavioural changes at runtime — the emitted Python is
identical to 0.1.6. Only `tyc check` / `tyc build` reject more
programs.

## 0.1.6

See the [v0.1.6 release notes](https://github.com/CodeHalwell/Typhon/releases/tag/v0.1.6).

Phase 5 — Interop and developer experience: `plain class` marker,
`Enum`/`Flag`/`ABC` auto-skip plus configurable
`[emit] skip-decoration-bases`, `class-default` validation,
`or`/`and` truthy-union typing, generator→`Iterable` conformance,
`tyc explain <code>` / `tyc cheatsheet`, upgraded `tyc init`,
`.py`-in-`src/` copy-through, `tyc build --check`,
`tyc::contains_secret_literal`, miette `url(...)` deep-links,
`tyc fmt` wrapping `ruff format`, `tyc debug --break <ty>:<line>`.

## Earlier

For history before 0.1.6 see `docs/roadmap.md` and the corresponding
GitHub release tags.
