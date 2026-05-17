# `vendor/` — Typhon's in-tree fork of Ruff's parser and AST

Five crates from [`astral-sh/ruff`](https://github.com/astral-sh/ruff) live
here as a permanent vendored fork:

| Crate | Upstream | Purpose |
|---|---|---|
| `ruff_text_size`     | `crates/ruff_text_size`     | `TextSize` / `TextRange` newtypes used everywhere as source offsets |
| `ruff_source_file`   | `crates/ruff_source_file`   | Line-index / column lookup over a source string |
| `ruff_python_trivia` | `crates/ruff_python_trivia` | Whitespace + comment skipping helpers |
| `ruff_python_ast`    | `crates/ruff_python_ast`    | Python AST + Typhon's `Mutability` extension |
| `ruff_python_parser` | `crates/ruff_python_parser` | Lexer + parser, plus the `let` / `mut` soft-keyword support |

The exact upstream revision is pinned in [`UPSTREAM`](./UPSTREAM).

## Typhon-specific extensions

Two source-level changes ride on top of the upstream code:

1. **`Mutability` enum + field** — `ruff_python_ast::Mutability` (`Let` /
   `Mut`) and a `mutability: Option<Mutability>` field on `StmtAssign`
   and `StmtAnnAssign`. Standard Python keeps `mutability = None`.
   Defined in `ruff_python_ast/src/nodes.rs`; the field is added to
   `generated.rs` and to the explicit destructurings in
   `comparable.rs`.
2. **`let` / `mut` soft keywords** — added to `TokenKind` in
   `ruff_python_ast/src/token.rs`, mapped from source text in
   `ruff_python_parser/src/lexer.rs`, and handled at statement-start
   position in `ruff_python_parser/src/parser/statement.rs::parse_simple_statement`.
   Mid-expression occurrences of `let` / `mut` continue to lex as
   identifiers, matching standard Python semantics.

Smoke tests at `vendor/ruff_python_parser/tests/typhon_mutability.rs` and
`crates/tyc-syntax/src/ruff.rs::tests` cover the new shapes.

## Build configuration

* Workspace `edition = "2021"` is preserved; each vendored crate sets
  `edition = "2024"` individually because the upstream sources depend on
  let-chains and other 2024-only syntax. `rust-version = "1.93"`
  matches upstream.
* `test = false` and `doctest = false` are set on the vendored libraries
  because upstream's own tests pull in dev-deps (`insta`,
  `datatest-stable`) that Typhon does not vendor. Typhon's own
  smoke tests are added as separate `[[test]]` entries.
* The vendored Cargo manifests pin every dependency at the version
  Ruff was using at the snapshot revision in `UPSTREAM`. When syncing
  forward, diff the upstream root `Cargo.toml` `[workspace.dependencies]`
  table against the manifests under `vendor/`.

## Migration status — consumer crates

The migration to the vendored Ruff parser/AST is **complete**.

| Stage | State |
|---|---|
| Vendor real source for ast/parser/trivia/source_file/text_size | ✅ done |
| Add `let` / `mut` soft keywords + `Mutability` field             | ✅ done |
| Expose ruff parser as `tyc_syntax::parse_module`                 | ✅ done |
| Port `tyc-syntax::preprocess` to stop stripping `let` / `mut`    | ✅ done (Step 8) |
| Port `tyc-resolve` to ruff AST                                   | ✅ done |
| Port `tyc-types` to ruff AST                                     | ✅ done |
| Port `tyc-analyse` to ruff AST                                   | ✅ done |
| Port `tyc-desugar` to ruff AST                                   | ✅ done |
| Port `tyc-emit` to ruff AST                                      | ✅ done |
| Port `tyc-db`, `tyc-format`, `tyc-lsp`, `tyc` CLI                | ✅ done |
| Drop `rustpython-parser` / `rustpython-ast` dependencies         | ✅ done (Step 9) |

Every consumer crate now uses `ruff_python_ast` (re-exported as
`tyc_syntax::ast`) and parses through the vendored Ruff parser via
`tyc_syntax::parse_module`. The transitional `tyc_syntax::parser` module
has been removed; there is no rustpython code path left in the workspace.

The let/mut keywords round-trip through the AST directly via
`StmtAssign.mutability` / `StmtAnnAssign.mutability`, and the resolver
reads mutability from those AST fields rather than the
`StrippedKeyword` side-channel. `StrippedKeyword` itself is kept for
the remaining Typhon-only line-prefix keywords that the parser still
doesn't know about (`model`, `impl`, `extend`, `interface`, `unsafe`,
`comptime`, `lazy`, `gather`, `go`) so the formatter can restore
them on output.

The hard part of the consumer swap is the AST shape diff. Significant
differences from `rustpython_ast`:

* `Constant::{Int, Float, Str, Bool, None, Ellipsis}` → `Expr::NumberLiteral`,
  `Expr::StringLiteral`, `Expr::BooleanLiteral`, `Expr::NoneLiteral`,
  `Expr::EllipsisLiteral` (each with its own struct).
* `Stmt::AsyncFunctionDef`, `Stmt::AsyncFor`, `Stmt::AsyncWith` are folded
  into the synchronous variants with an `is_async: bool` field.
* `Arg`, `ArgWithDefault` → `Parameter`, `ParameterWithDefault`.
* AST nodes are not generic over the range type; `rustpython_ast::Stmt<TextRange>`
  becomes `ruff_python_ast::Stmt`.
* Every AST node gains a `range: TextRange` and a `node_index: AtomicNodeIndex`
  field. Constructors must populate both.
* `Mod::FunctionType` does not exist in `ruff_python_ast`.
* `Identifier::new(s)` → either `Identifier { id: Name::new(s), range, node_index }`
  or `Identifier::new(s, range)` depending on the constructor used.

## Syncing with upstream

1. Bump `UPSTREAM` to the new revision SHA.
2. `cd /tmp && git clone --depth=1 https://github.com/astral-sh/ruff`
   and `git -C /tmp/ruff checkout <new-sha>`.
3. For each vendored crate, diff the upstream `src/` against the local
   tree and reapply Typhon's extensions on top:
   - `ruff_python_ast::Mutability` enum + struct fields
   - `TokenKind::Let` / `TokenKind::Mut` and the `is_keyword` /
     `is_soft_keyword` bounds
   - `lexer.rs` keyword table entries
   - `parser/statement.rs::parse_simple_statement` let/mut dispatch
4. Re-run `cargo test -p ruff_python_parser --test typhon_mutability`
   and `cargo test -p tyc-syntax`.
