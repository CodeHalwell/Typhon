# Follow-ups after the May 2026 completion sprint

This document tracks what landed in this branch
(`claude/assess-project-completion-gUdOP`), what was deliberately deferred,
and the rationale.  Read alongside `docs/roadmap.md` for the canonical
phase status.

## What landed

| Area | Change |
|---|---|
| Syntax | Module-level `lazy val NAME: T = expr` lowers to `lazy_val(lambda: expr)`; class-body `lazy val` lowers to `@cached_property`. Both round-trip through `tyc fmt`. |
| Syntax | `extend BUILTIN:` is rejected at preprocess time with a dedicated `tyc::extend_builtin` diagnostic; user-defined classes still flow through `impl`-merge. |
| Types | `unsafe:` blocks now bump `Checker::unsafe_depth` via preprocess line metadata; type-mismatch, nullable-use, interface-isinstance, wrong-arg-count, not-callable, and non-exhaustive-match diagnostics are dropped inside the block. Errors on lines outside the block are unaffected. |
| Types | `Type::TypeVar(name)` replaces `Type::Any` for PEP 695 type parameters in signatures; call-site `bind_typevars_and_substitute` infers bindings (recursively, with conflict-widening) and substitutes them in the return type. |
| CLI | `tyc trace` reads `.py.map` sidecars emitted by `tyc build` and rewrites `File "…/foo.py", line N` traceback entries to point at the original `.ty`. |
| CLI | `tyc profile` post-processes the build output with a `@__typhon_profile_record` decorator on every top-level function plus a generated `typhon_profile.py` helper that flushes call counts and total wall-clock time to `typhon-profile.json` on `atexit`. |
| CLI | `tyc migrate` converts typed Python (`.py`) to Typhon (`.ty`): `Optional[T]` / `T \| None` → `T?`, module-level annotated assigns gain `val`/`var` (val unless later reassigned), `@dataclass` decorators and the `dataclass` import are dropped. |
| LSP | `Backend` caches the latest text per URI on `did_open`/`did_change` so hover and go-to-definition can resolve without re-fetching from the editor. |
| LSP | `textDocument/hover` returns the binding kind + mutability rendered in markdown; `textDocument/definition` jumps to the resolver's recorded declaration span. Both rely on a new `ResolvedModule::symbol_at_offset` query. |
| Stubs | `tyc check --stubs` now diffs each `.dty`'s public API against its sibling `.ty` (or `.py`) implementation via a new `tyc_emit::compare_modules`; mismatches surface as `tyc::stub_mismatch` diagnostics. Private names (leading underscore) are excluded by design. |

Test coverage: 514 unit tests across the workspace (up from 371 at the
start of the sprint).  `cargo fmt --check`, `cargo clippy
--workspace --all-targets -- -D warnings`, and `cargo test --workspace`
all pass.

## Deliberately deferred

### Ruff parser fork

Still on `rustpython-parser` 0.4 from crates.io.  The fork is a
multi-day effort even with focused scope: vendor
`ruff_python_parser` + `ruff_python_ast` + the small slice of
`ruff_text_size` they need, add the two extension tokens (`val`,
`var`), get the `ruff_python_codegen` round-trip working on a
representative corpus, then swap the dependency across every
consumer crate.  Attempting it in this session would have produced
an incomplete vendor tree, broken parsing, and a long red CI.

What we have today: hand-written `tyc-emit` printer covers the
Python subset used by every emitted file, all current tests pass,
and the preprocessor handles the Typhon-specific keyword surface.
The fork is therefore an optimisation/cleanup project, not a
blocker.

### Auto-gather inference and loop parallelisation

Phase 4 features in the long-term plan.  Both require interaction
with the purity engine (already landed) and the type checker to
prove that the candidate region is safe — fundamentally new
analysis passes, not local tweaks.  Not attempted in this sprint.

### PEP 695 inference depth

`bind_typevars_and_substitute` solves the common case (one typevar
per signature, two-way unification through generic containers).
Bounded type vars (`type T = T: Comparable`), variance
considerations, and higher-kinded forms are not handled — the
engine still treats those positions permissively.  Multi-stmt
inference across helper calls also remains an aspiration.

### Source-map line accuracy

`.py.map` records the source path only; line offsets are forwarded
1:1.  Most preprocessing preserves line counts (val/var, comptime,
lazy val, optional sugar), but `with`-chains, `gather:`, and `?`
propagation emit multiple Python lines from one Typhon line, so
tracebacks pointing at those constructs may report a line offset
by a small amount.  A proper line-array map (à la JS source maps
v3) lands later.

### Runtime stubtest probe

`tyc check --stubs` performs an AST diff today.  mypy's `stubtest`
proper imports the module at runtime and inspects attributes via
introspection, which catches dynamically-created members the AST
cannot see.  Adding a sandboxed runtime probe is a follow-up; the
AST diff already covers the most common drift sources (rename,
delete, signature change).

## Recommended next steps, in order

1. Vendor the Ruff parser fork.  The longer it slides, the more
   workarounds accumulate in `tyc-syntax/preprocess.rs`.
2. Promote `bind_typevars_and_substitute` into a structural
   sub-type checker.  The substitution table is the right
   anchor for `where T: SomeInterface` once it lands.
3. Expand the source-map format to a `(out_line → ty_line)`
   table emitted from the printer.  `tyc trace` already uses the
   file once it exists; the missing piece is the writer.
4. Replace the documents cache in `tyc-lsp` with a salsa-tracked
   per-file input so hover/definition share the same incremental
   db as `check_file`.
