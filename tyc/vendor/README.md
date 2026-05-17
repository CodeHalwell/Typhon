# `vendor/` — Ruff parser fork (scaffold)

This directory holds the in-tree fork of `ruff_python_parser` and
`ruff_python_ast` that the [roadmap](../../docs/roadmap.md#phase-0--foundation-months-12--substantially-complete)
calls out as the Phase-0 follow-up.

**Current state: scaffold only.** The two crates compile as empty stubs
and are declared in the workspace `members` list, but no Typhon crate
depends on them yet. Production Typhon still routes through
`rustpython-parser` 0.4 from crates.io, and every test passes against
that parser.

## Why a fork

Typhon needs two custom lexer tokens — `val` and `var` — that signal
mutability on assignment statements. Today the
[preprocessor](../crates/tyc-syntax/src/preprocess.rs) strips these
prefixes from the input before handing the result to `rustpython-parser`
so the parser only ever sees standard Python. That trick works but it
limits us:

- Column-accurate diagnostics on the `val`/`var` keyword itself are
  awkward because the parser doesn't know they were ever there.
- Adding richer syntax (e.g. structural sugar that depends on lexer
  context) requires more preprocessor gymnastics.
- The `rustpython-parser` AST has diverged from upstream CPython
  semantics in small ways (no PEP 695 inference, older `match`
  pattern shapes). Ruff's parser tracks CPython more closely and is
  the parser most other tools in the Python static-analysis ecosystem
  (pyrefly, ty, pyright bindings) use.

## Migration plan

1. **Vendor source.** Copy the upstream Ruff workspace's
   `crates/ruff_python_ast/` and `crates/ruff_python_parser/`
   directories into here, replacing the scaffold `src/lib.rs` files.
   Pull in the slice of `ruff_text_size` they depend on as a third
   vendored crate if needed.
2. **Add `val` / `var` tokens.** Extend the lexer's keyword table and
   parser productions so `val x: int = 1` and `var y = 2` produce a
   `StmtAssign`/`StmtAnnAssign` node with a new `mutability` field.
   Update the corresponding `ruff_python_codegen::Generator` paths so
   the tokens round-trip on emit.
3. **Round-trip corpus.** Re-run the round-trip test against a real
   Python corpus (Django management commands per the roadmap) and
   confirm byte-identical output modulo whitespace.
4. **Swap consumers.** Each of `tyc-syntax`, `tyc-types`,
   `tyc-resolve`, `tyc-desugar`, `tyc-emit`, `tyc-analyse`, and
   `tyc/src/commands/*` currently `use rustpython_ast::*` and
   `use rustpython_parser::*`. Swap those to `use ruff_python_ast::*`
   and `use ruff_python_parser::*` and fix the field/variant
   differences. The Ruff AST is similar but not identical
   (different `Constant` representation, different range types, no
   `Mod::FunctionType` variant, etc.).
5. **Port the hand-written emitter.** `tyc-emit/src/printer.rs`
   currently targets `rustpython_ast` node shapes. Either port to
   Ruff's shapes or call `ruff_python_codegen::Generator` directly.
6. **Update tests.** ~600 tests reference parser APIs; bulk-rename
   them and fix any AST-shape assumptions.

Realistic effort: 2–4 working days of focused implementation. The
tree will be partially broken during the migration; do it on a
dedicated branch.

## Why the scaffold exists now

So the workspace topology and crate boundaries are already in place
when the migration runs. Adding the real source is a drop-in
replacement of two `src/lib.rs` files rather than a workspace edit
plus dependency-graph reshuffling.

The scaffold crates are compiled by `cargo build --workspace` so
the dependency edge stays healthy, but they export only a
`SCAFFOLD_MARKER` string that tests can assert on to detect a
half-finished swap.
