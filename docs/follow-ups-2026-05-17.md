# Follow-ups after the May 2026 completion sprint

This document tracks what landed in this branch
(`claude/assess-project-completion-gUdOP`), what was deliberately deferred,
and the rationale.  Read alongside `docs/roadmap.md` for the canonical
phase status.

## What landed

| Area | Change |
|---|---|
| Syntax | Module-level `lazy let NAME: T = expr` lowers to `lazy_let(lambda: expr)`; class-body `lazy let` lowers to `@cached_property`. Both round-trip through `tyc fmt`. |
| Syntax | `extend BUILTIN:` is rejected at preprocess time with a dedicated `tyc::extend_builtin` diagnostic; user-defined classes still flow through `impl`-merge. *(Update: built-in extensions have since landed — `extract_builtin_extensions` lowers each method to a free function and rewrites annotated call sites. The `validate_extend_usage` validator is now a no-op kept for back-compat with the diagnostic enum.)* |
| Types | `unsafe:` blocks now bump `Checker::unsafe_depth` via preprocess line metadata; type-mismatch, nullable-use, interface-isinstance, wrong-arg-count, not-callable, and non-exhaustive-match diagnostics are dropped inside the block. Errors on lines outside the block are unaffected. |
| Types | `Type::TypeVar(name)` replaces `Type::Any` for PEP 695 type parameters in signatures; call-site `bind_typevars_and_substitute` infers bindings (recursively, with conflict-widening) and substitutes them in the return type. |
| CLI | `tyc trace` reads `.py.map` sidecars emitted by `tyc build` and rewrites `File "…/foo.py", line N` traceback entries to point at the original `.ty`. |
| CLI | `tyc profile` post-processes the build output with a `@__typhon_profile_record` decorator on every top-level function plus a generated `typhon_profile.py` helper that flushes call counts and total wall-clock time to `typhon-profile.json` on `atexit`. |
| CLI | `tyc migrate` converts typed Python (`.py`) to Typhon (`.ty`): `Optional[T]` / `T \| None` → `T?`, module-level annotated assigns gain `let`/`mut` (let unless later reassigned), `@dataclass` decorators and the `dataclass` import are dropped. |
| LSP | `Backend` caches the latest text per URI on `did_open`/`did_change` so hover and go-to-definition can resolve without re-fetching from the editor. |
| LSP | `textDocument/hover` returns the binding kind + mutability rendered in markdown; `textDocument/definition` jumps to the resolver's recorded declaration span. Both rely on a new `ResolvedModule::symbol_at_offset` query. |
| Stubs | `tyc check --stubs` now diffs each `.dty`'s public API against its sibling `.ty` (or `.py`) implementation via a new `tyc_emit::compare_modules`; mismatches surface as `tyc::stub_mismatch` diagnostics. Private names (leading underscore) are excluded by design. |

Test coverage: 514 unit tests across the workspace (up from 371 at the
start of the sprint).  `cargo fmt --check`, `cargo clippy
--workspace --all-targets -- -D warnings`, and `cargo test --workspace`
all pass.

## Deliberately deferred

### Cross-module constructor / method arity checks — ✅ landed

Cross-module constructor + method arity checks now fire for
`from foo import Bar` style imports — both `.ty` source and `.dty`
stubs flow through the same project-wide shape registry. The CLI
(`tyc check` / `tyc build`) builds the registry once per invocation;
the LSP rebuilds it on every check so the editor reacts to changes in
sibling files within one keystroke.

Limitations of the current implementation, tracked for a follow-up:

- Bare `import M as N` followed by `N.SomeClass(...)` dotted access
  isn't yet wired; the local alias `N` lands as `Type::Unknown` and
  the constructor call bypasses the check. The CLI catches this via
  the unknown-module pass; the type-level fix needs module-object
  modelling.
- The LSP rebuilds the registry on every check (parse-only walk of
  the project's `.ty` / `.dty` tree). For large projects this could
  become noticeable; a Salsa-tracked cache keyed on file-text would
  be the natural follow-up.

### Post-construction field-init audit — ✅ landed (tight scope)

A new `tyc::missing_field_init` diagnostic catches the
`X.__new__(X)` / `object.__new__(X)` bypass-construction patterns: if
the instance escapes the function (return / call argument) without
every required field assigned, the audit fires. Skipped inside
`unsafe:` blocks. Dropped conservatively on `setattr`, on method
calls, and on rebinding — false negatives are preferred to false
positives.

Out of scope (documented in the diagnostic's help text):

- Container-literal escapes (`return [c]`, `return {"x": c}`).
- Outer-scope assignment escapes.
- Interprocedural reasoning (a method that does initialise fields
  suppresses the audit; the audit can't tell which).
- Subclass field tracking.

### Ruff parser fork — ✅ landed since this sprint

This follow-up has been resolved. `ruff_python_parser`,
`ruff_python_ast`, `ruff_python_trivia`, `ruff_source_file`, and
`ruff_text_size` are vendored under `tyc/vendor/` with `let`/`mut`
soft-keyword support and a `Mutability` field on assignment AST
nodes. Every consumer crate now parses through
`tyc_syntax::parse_module`; the `rustpython-parser` dependency has
been removed. See `tyc/vendor/README.md` for the migration record.

`ruff_python_codegen` was *not* vendored — `tyc-emit` retains its
hand-written printer because upstream codegen does not expose the
per-statement line-offset hook required for `.py.map` source maps.
Vendoring it remains an open optional follow-up.

### Auto-gather inference and loop parallelisation — ✅ landed since this sprint

Both have shipped behind `[strictness]` opt-ins. Straight-line
independent `await` runs inside an `async def` fold into an
`asyncio.TaskGroup` when `auto-gather = true`; pure list
comprehensions over an iterable that meets `parallel-min-size`
rewrite to a thread-pool map when `auto-parallel = true`. Both
respect the six-condition purity check and only fire when the
candidate region is statically safe.

### PEP 695 inference depth

`bind_typevars_and_substitute` solves the common case (one typevar
per signature, two-way unification through generic containers).
Bounded type vars (`type T = T: Comparable`), variance
considerations, and higher-kinded forms are not handled — the
engine still treats those positions permissively.  Multi-stmt
inference across helper calls also remains an aspiration.

### Source-map line accuracy — ✅ landed since this sprint

The printer now records a per-statement `line_offsets` table while
emitting; `.py.map` v2 stores a `(out_line → ty_line)` mapping that
`tyc trace` reads to rewrite tracebacks at line granularity, even
across multi-line expansions (`with`-chains, `gather:`, `?`
propagation). The format is JS-source-maps-v3-shaped enough for the
LSP's cross-file go-to-definition path to consume it too.

### `class!` raw-class modifier — prototype landed, polish outstanding

**Status: prototype implemented.** A `class!` modifier now suppresses
the automatic `@dataclass` decorator injection at desugar time. The
preprocessor strips the `!`, records the line in
`PreprocessResult::raw_class_lines`, and `desugar_module_with` skips
the decorator (and the `import dataclasses` injection) for any class
whose `TextRange` starts at one of those byte offsets. Round-trips
through `tyc fmt`. See `crates/tyc-syntax/src/preprocess.rs`,
`crates/tyc-desugar/src/lib.rs:973`, and the regression tests
`raw_class_strips_bang_and_records_line`,
`raw_class_round_trips_via_postprocess`, and
`raw_class_skips_dataclass_decorator`.

The motivating problem: subclassing `torch.nn.Module`, `enum.Enum`,
`typing.NamedTuple`, `unittest.TestCase`, or framework declarative
bases (Django, SQLAlchemy) all need a non-trivial `__init__` that
runs *before* field assignment. The auto-injected dataclass
`__init__` never calls `super().__init__()`, so e.g. `nn.Module._parameters`
is never initialised and the first attribute assignment crashes
inside `Module.__setattr__`.

Working example:

```typhon
import torch.nn as nn

class! MyModel(nn.Module):
    layer: nn.Linear
    dropout: float

    def forward(self, x):
        return self.layer(x)
```

The desugar pass now **auto-generates `__init__`** for any `class!`
that has at least one positional base and no hand-written `def
__init__` in the body. The synthesised constructor calls
`super().__init__()` and then assigns every annotated field through
`self`, in source order. Field defaults flow into the parameter
signature. Hand-written `__init__` blocks are preserved verbatim. So
the example above lowers to:

```python
class MyModel(nn.Module):
    layer: nn.Linear
    dropout: float

    def __init__(self, layer: nn.Linear, dropout: float) -> None:
        super().__init__()
        self.layer = layer
        self.dropout = dropout

    def forward(self, x):
        return self.layer(x)
```

Outstanding before this can ship as a documented feature:

- **Language guide.** No mention in `docs/language.md` yet — needs a
  short section explaining when to reach for `class!` vs. `class` /
  `model` / `interface`.
- **Type-checker integration.** A `class!` body still gets the same
  treatment as a plain `class` in `tyc-types`; in particular the
  field-initialisation requirement no longer applies (the user's
  `__init__` does the work), but the checker doesn't know that. Today
  this is fine because field annotations on a raw class compile to
  bare class-level annotations, but a future check that warns on
  missing initialisers needs to be class-kind-aware.
- **`tyc migrate` symmetry.** Going `.py` → `.ty`, classes with an
  explicit `__init__` that doesn't match the dataclass shape should
  emit as `class!` rather than `class`. Currently `migrate` always
  emits plain `class`.
- **LSP hover.** `class!` declarations should advertise themselves in
  hover output the way `mut`/`let` bindings do.
- **Cross-module raw-class tracking.** The byte-offset list is
  per-module today. If a future pass needs to know "is this class
  raw?" from a different module, the resolver should propagate the
  flag onto the binding metadata rather than rely on line lookup.

### Runtime stubtest probe

`tyc check --stubs` performs an AST diff today.  mypy's `stubtest`
proper imports the module at runtime and inspects attributes via
introspection, which catches dynamically-created members the AST
cannot see.  Adding a sandboxed runtime probe is a follow-up; the
AST diff already covers the most common drift sources (rename,
delete, signature change).

## Recommended next steps, in order

> **Update (May 2026 — second pass):** items 3, 5, and 6 below have all
> landed since the last revision. The current list of open follow-ups is:

1. ✅ **Variance landed** — `assignable` now consults a hand-curated
   `generic_param_variance(head, idx)` table for every generic
   parameter, applying covariance / contravariance / invariance per
   position instead of recursing covariantly everywhere. Mutable
   containers (`list`, `set`, `dict[K, V]` both axes, the `Mutable*`
   ABCs) are invariant; read-only views (`Sequence`, `Iterable`,
   `Mapping[V]` value position, …) are covariant; `Callable` is
   contravariant in arguments and covariant in return; user-defined
   generics default to invariant for soundness. Unblocks the soundness
   bug where `list[int]` previously flowed into `list[float]` under
   numeric widening. See `tyc-types/src/lib.rs::assignable` and
   `generic_param_variance`. Higher-kinded forms (`T[U]` where `T`
   is itself a type variable) remain partial — rare in practice and
   the substitution table handles them permissively.

2. **Vendor `ruff_python_codegen` — deliberately not done.** Probed in
   May 2026 and concluded the cost outweighs the benefit:

   - Upstream `ruff_python_codegen` has no per-statement `line_offsets`
     hook. Adding one means instrumenting every newline emit site
     (~36 in upstream `generator.rs` at the pinned revision), which
     is the same amount of work as maintaining the hand-written
     printer.
   - Upstream pulls in `ruff_python_literal` for string escapes,
     which transitively pulls `icu_properties` — a sizable
     dependency just for Unicode classification.
   - Vendored ruff_python_ast has Typhon's `Mutability` extension;
     to emit `let`/`mut` round-trip, upstream codegen needs patched
     too — defeating the "use upstream as-is" benefit.

   Net: vendoring would *replace* a working 1.6 kLOC printer with a
   patched 2.5 kLOC vendor (plus dependencies) for zero functional
   gain. The right call is to keep the hand-written printer; the
   sync burden a vendor introduces wouldn't be earned.

   If the picture changes (upstream adds a public line-offset hook,
   the `let`/`mut` extension lands upstream as a feature flag), the
   vendor becomes attractive — until then, leave it alone.
3. ✅ **Runtime `stubtest` probe** — landed as `tyc stubtest`. The
   command builds the project, walks the output for `.pyi` stubs,
   derives Python import paths, and invokes
   `python -m mypy.stubtest <module>` with `PYTHONPATH` pointing at
   the build directory. Flags mirror `tyc ty` where they overlap;
   `--keep-going` gets the full drift report in CI rather than
   stopping at the first failure. Documented in `docs/cli.md` and
   surfaced in the README's subcommand table.
4. **`ty` integration as a complementary second-stage checker.**
   Phase 1 (subprocess `tyc ty`) ships. Phase 2 (embedded library
   sharing the Salsa db) was probed in May 2026 and deferred —
   what's required, in order:

   - Vendor `ty_python_semantic` plus its workspace siblings: `ruff_db`,
     `ruff_diagnostics`, `ruff_index`, `ruff_macros`,
     `ruff_memory_usage`, `ruff_python_literal`, `ruff_python_stdlib`,
     `ty_module_resolver`, `ty_site_packages`, `ty_python_core`.
     Roughly 8 new crates on top of the 5 already vendored.
   - `ty_python_semantic` consumes `ruff_python_ast` — Typhon's
     fork has a `Mutability` extension on assignment AST nodes that
     upstream doesn't have. Either keep the fork compatible with
     upstream's expected shape (workable: the extension is an
     additive field) or maintain a translation layer.
   - `ty`'s public API is alpha pre-1.0; the integration doc
     explicitly recommends pinning a commit and treating each upgrade
     as a deliberate sync. The first sync after embedding will be
     instructive — likely several days of effort.
   - The Salsa-db sharing in `docs/ty-integration.md` Phase 2 Step 2.3
     wants both checkers' queries to invalidate together. That
     requires `ty_python_semantic::Db` to be impl'd on `TycDatabase`,
     which in turn requires the vendored ty crate to compile in
     this workspace — non-trivial because ty uses workspace-level
     deps Typhon doesn't declare.

   The subprocess path (`tyc ty`) covers the practical "second
   opinion" use case today. Phase 2 should land when (a) `ty` ≥ 1.0
   so the API is stable, or (b) a Typhon program actually needs
   sub-100ms incremental ty checking (the subprocess re-invokes ty's
   parser each time). Neither is true today.
5. ✅ **Richer comptime — `comptime def` functions.** The evaluator
   now dispatches into user-declared `comptime def` helpers from
   any `comptime let` RHS. v1 body grammar covers `return EXPR`,
   local `let`/`mut`/plain assignments, `if`/`elif`/`else`, the
   ternary `EXPR if COND else EXPR`, comparisons, and short-circuit
   `and`/`or`/`not`. Recursion depth is capped at 64. Loops, exceptions,
   `with`-blocks, and `class`/`def` declarations stay rejected.
   Examples in `docs/language.md`. Types as values remain future
   work.
6. ✅ **`class!` polish** — documented in `docs/language.md`,
   surfaced in LSP hover (rendered as "raw class (`class!`)"), and
   `tyc migrate` now promotes a `class Name(...)` declaration to
   `class! Name(...)` whenever the body declares a hand-written
   `def __init__` at the immediate body indent and the class isn't
   already opting into dataclass semantics via `@dataclass`. The
   resolver carries a `ClassKind { Plain, Raw }` on every class
   binding so downstream passes can branch on the marker without
   re-scanning byte ranges; cross-file go-to-definition jumps
   carry the metadata along, so hover after a jump still renders
   the correct kind.

## May 19 stress-test pass — what landed

The 2026-05-19 stress-test campaign (see `FINDINGS.md` #57–#96)
surfaced a fresh batch of bugs in the corners of the language. The
following landed since the May 17 follow-ups; the rest remain
tracked in `FINDINGS.md`.

| Finding | Area | What changed |
|---|---|---|
| #57 / #58 / #70 | Types | `type Alias = …` declarations are now transparent during assignability — including generic aliases (`type StringMap[V] = dict[str, V]`) and union aliases (`type B = int | str`). Cycles terminate at depth 8. |
| #59 | Generics | Method-level type parameters on `impl[T]` blocks (`def map[U](self, f: Callable[[T], U])`) resolve correctly; both impl-level and method-level params share the function scope. |
| #60 | Async | `gather:` blocks whose bindings reference earlier bindings in the same block fall back to a sequential `let x = await …` lowering instead of producing broken Python that would `UnboundLocalError` at runtime. Independent bindings keep the `TaskGroup` lowering. |
| #61 | Resolve | `global` / `nonlocal` declarations are now respected by `tyc::missing_binding_kind`: names declared global/nonlocal in a function skip the let/mut requirement (the outer-scope binding owns the kind). |
| #62 | Desugar | Mutable defaults in dataclass-backed `class` fields (`tags: list[str] = []`) are rewritten to `dataclasses.field(default_factory=<ctor>)` automatically. Skipped for `model`/`interface`/`class!`. |
| #63 | Types | `@property` on an `impl`-block method types the attribute access as the property's return type, not the underlying `() -> T` callable. `let area: float = r.area` now type-checks. |
| #64 | CLI | `tyc migrate` now adds `let` to function-body plain assignments (first-occurrence detection is per-function), promoted to `mut` when the name is reassigned anywhere in the file. The reassignment flag is file-wide so it deliberately over-approximates to `mut`. Output passes `tyc check` on first try. |
| #65 (partial) | CLI | `tyc fmt` v1 now collapses runs of internal whitespace and tidies bracket/comma spacing (`def    main(  x  ,    y  )` → `def main(x, y)`). Spacing around `:`, `=`, `->` still left alone — those need bracket-depth awareness and are deferred to the AST-based reprinter (Phase 5). |
| #66 (diagnostic) | Syntax | Mid-expression `?` (`return Ok(step(x)?)`) emits a targeted `tyc::invalid_question_op` diagnostic with a span in the user's source explaining "lift the inner call to a `let` binding first". The actual mid-expression lowering remains a follow-up. |
| #84 | Desugar | `lazy let` now triggers the `typhon_runtime` package emission via `has_any_typhon_runtime_import`, which matches both the bare module import and every `typhon_runtime.<sub>` submodule (the lazy-let lowering uses `typhon_runtime.lazy`). |
| `class!` polish | Desugar | When `class!` synthesises `__init__`, class-level field defaults are stripped from the body (the default is carried only in the generated parameter list). Annotations survive. Avoids double-evaluation that would, for example, register dead `nn.Linear` instances on a PyTorch subclass. |
