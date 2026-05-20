# Roadmap

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Realistic milestones for one person plus AI assistance. The headline target is a useful subset shippable in twelve months.

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
  see https://typhon.dev/lang/impl for the full pattern
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
  is a same-module `async def` and the awaits are statically independent.
  Opt-in via `[strictness] auto-gather = true`. The desugar pass injects
  `import asyncio` if missing.
- Loop parallelisation for pure comprehensions on free-threaded Python.
- Richer comptime: `comptime` functions, types as values.
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
- Migration tooling from typed `.py` to `.ty` (`Optional[T]` → `T?`, dataclasses → Typhon classes, etc.).
- **`ty` integration** as a complementary second-stage checker over the
  desugared Python. Planned in two phases: first as a subprocess
  invocation of `ty check` with diagnostic attribution via the source
  maps (no dependency on the Ruff vendor), later as an embedded
  library sharing the Salsa db. See [docs/ty-integration.md](ty-integration.md)
  for the full plan.

## Scope-cutting rule

The minimum-viable Typhon is **non-null types + sealed unions + `Result` + dataclass emit**. That alone is publishable. Everything else can be sacrificed to ship.

## Concrete next steps

Phases 0–3 are complete. The current frontier is Phase 4+:

1. Corpus round-trip sweep: run `tyc build` over a representative set of
   third-party Python projects and compare the emitted `.py` against the
   source semantically. Not a blocker (the test suite is green), but
   hardens confidence.
2. Promote `bind_typevars_and_substitute` into a proper structural
   sub-type checker that handles variance and bounded higher-kinded forms.
3. Expand the Salsa boundary: make `resolve_module` and `check_module` into
   Salsa-tracked queries so the LSP second-check latency drops to near-zero
   for unchanged files.
4. Loop parallelisation for pure comprehensions on free-threaded Python.
5. Runtime `stubtest` probe via `mypy --stubtest` as a complement to the
   AST-level `tyc check --stubs` diff.
