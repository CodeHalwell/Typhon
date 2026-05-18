# Follow-ups after the May 2026 completion sprint

This document tracks what landed in this branch
(`claude/assess-project-completion-gUdOP`), what was deliberately deferred,
and the rationale.  Read alongside `docs/roadmap.md` for the canonical
phase status.

## What landed

| Area | Change |
|---|---|
| Syntax | Module-level `lazy let NAME: T = expr` lowers to `lazy_val(lambda: expr)`; class-body `lazy let` lowers to `@cached_property`. Both round-trip through `tyc fmt`. |
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

### Runtime stubtest probe

`tyc check --stubs` performs an AST diff today.  mypy's `stubtest`
proper imports the module at runtime and inspects attributes via
introspection, which catches dynamically-created members the AST
cannot see.  Adding a sandboxed runtime probe is a follow-up; the
AST diff already covers the most common drift sources (rename,
delete, signature change).

## Recommended next steps, in order

> **Update (May 2026):** items 1, 3, and 4 below have all landed.
> The current list of open follow-ups is:

1. Promote `bind_typevars_and_substitute` into a full structural
   sub-type checker. Bounded type-var checking and basic
   conformance now use the substitution table; variance and
   higher-kinded forms remain partial.
2. Vendor `ruff_python_codegen` to replace `tyc-emit/src/printer.rs`,
   preserving the `line_offsets` hook the hand-written printer
   exposes today (see `tyc/vendor/README.md`).
3. Runtime `stubtest` probe via `mypy --stubtest` as a complement
   to the AST-level `tyc check --stubs` diff. Catches dynamically
   created members the AST cannot see.
4. `ty` integration as a complementary second-stage checker (see
   `docs/ty-integration.md`).
5. Richer comptime: `comptime` functions, types as values.
