# `vendor/` — Ruff parser fork (scaffold)

This directory holds the in-tree fork of `ruff_python_parser` and
`ruff_python_ast` that the [roadmap](../../docs/roadmap.md#phase-0--foundation-months-12--substantially-complete)
calls out as the Phase-0 follow-up.

**Current state: scaffold only.** The two crates compile as empty stubs
and are declared in the workspace `members` list, but no Typhon crate
depends on them yet. Production Typhon still routes through
`rustpython-parser` 0.4 from crates.io, and every test passes against
that parser.

---

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

---

## Migration plan — step-by-step

This is **not** an in-place edit. Do the work on a dedicated branch
(`vendor-ruff`) with the expectation that CI stays red for the duration.
Each numbered step below is one logical commit.

### Pre-flight

Pick an upstream Ruff revision and pin it in `vendor/UPSTREAM`:

```
echo "astral-sh/ruff @ <commit-sha>" > vendor/UPSTREAM
git -C /tmp clone --depth=1 https://github.com/astral-sh/ruff
```

Skim `crates/ruff_python_parser/CHANGELOG.md` and
`crates/ruff_python_ast/CHANGELOG.md` since Typhon's last sync so you
know what semantic changes ride along.

### Step 1 — Vendor `ruff_python_ast` (real source)

```
rm -rf vendor/ruff_python_ast/src
cp -R /tmp/ruff/crates/ruff_python_ast/src vendor/ruff_python_ast/src
cp /tmp/ruff/crates/ruff_python_ast/Cargo.toml \
   vendor/ruff_python_ast/Cargo.toml.upstream
```

Then merge `Cargo.toml.upstream` into the existing scaffold `Cargo.toml`:

- Keep our `package.name`, `version`, `edition.workspace = true`,
  `license.workspace = true`, `publish = false`.
- Copy upstream's `dependencies` block verbatim; replace any
  `ruff_text_size = { workspace = true }` with the path-dep added in
  Step 3 below.
- Drop the `dev-dependencies` block for now — the upstream tests need
  more vendored crates than we want to pull in. (Add them back later
  as their own opt-in feature.)

Verify with `cargo build -p ruff_python_ast`. Expect compile errors
about missing `ruff_text_size`; that lands next.

### Step 2 — Vendor `ruff_text_size`

`ruff_python_ast` and `ruff_python_parser` both depend on
`ruff_text_size`. It's a 300-LOC crate that just wraps `text-size`
with `serde` impls, so vendor it as its own member:

```
mkdir -p vendor/ruff_text_size
cp -R /tmp/ruff/crates/ruff_text_size/src vendor/ruff_text_size/src
```

Write `vendor/ruff_text_size/Cargo.toml` mirroring the scaffold pattern.
Add `"vendor/ruff_text_size"` to the workspace `members` list in
`tyc/Cargo.toml`. Run `cargo build -p ruff_python_ast` again; the AST
crate should now build cleanly.

### Step 3 — Vendor `ruff_python_parser` (real source)

Same drill as Step 1 but for the parser. Replace
`vendor/ruff_python_parser/src/lib.rs` with the upstream source
(usually a single-file re-export over `crates/ruff_python_parser/src/`,
which copies wholesale). The parser depends on `ruff_python_ast` and
on a handful of other Ruff helpers (`ruff_source_file`,
`unicode_ident`, etc.); vendor each one as its own crate as needed
until `cargo build -p ruff_python_parser` is green.

Estimate: 4–6 small helper crates, ≤ 200 LOC each.

### Step 4 — Round-trip the unmodified parser

Add a smoke test under `vendor/ruff_python_parser/tests/roundtrip.rs`
that parses a representative Python corpus and re-emits it via
`ruff_python_codegen` (which you'll need to vendor too — or just
inspect the AST and confirm parse succeeded).

```rust
#[test]
fn parses_django_management_commands() {
    for path in std::fs::read_dir("tests/corpus/django").unwrap() {
        let src = std::fs::read_to_string(path.unwrap().path()).unwrap();
        let module = ruff_python_parser::parse_module(&src).unwrap();
        assert!(!module.syntax().body.is_empty());
    }
}
```

Drop a small corpus into `vendor/ruff_python_parser/tests/corpus/`
(MIT-licensed sources only — note licenses in `vendor/CORPUS_LICENSE`).
This step proves the vendor copy works end-to-end before we start
modifying the lexer.

### Step 5 — Add `val` / `var` tokens

Modify `vendor/ruff_python_parser/src/lexer.rs`:

1. Extend the keyword table (the static `HashMap` or `phf::Map`) with
   `"val"` and `"var"` mapping to a new `Tok::Val` / `Tok::Var` variant
   (add the variants to `Tok` in the same file).
2. Update the lexer's identifier classifier to emit these new tokens
   when a bare `val`/`var` appears at statement-start position.
   Keep them recognised only at the start of a logical line so an
   identifier `val` mid-expression remains an identifier (Python
   compatibility).

Modify `vendor/ruff_python_ast/src/nodes.rs`:

3. Add `pub mutability: Option<Mutability>` to `StmtAssign` and
   `StmtAnnAssign`, with `enum Mutability { Val, Var }`.

Modify `vendor/ruff_python_parser/src/parser/statement.rs`:

4. In the assignment-statement production, peek for `Tok::Val`/`Tok::Var`
   at the start. If present, set the new `mutability` field; otherwise
   leave `None`. Standard Python keeps `None` and parses unchanged.

Tests in `vendor/ruff_python_parser/tests/`:

```rust
#[test]
fn val_assignment_carries_mutability() {
    let m = ruff_python_parser::parse_module("val x: int = 1\n").unwrap();
    let Stmt::AnnAssign(a) = &m.syntax().body[0] else { panic!() };
    assert_eq!(a.mutability, Some(Mutability::Val));
}
```

### Step 6 — Port `ruff_python_codegen` (or skip it)

Two paths:

**(a)** Vendor `ruff_python_codegen` and extend its statement printer
to prefix `val ` / `var ` when `mutability` is set. ~50 LOC change,
cleanest result.

**(b)** Keep Typhon's hand-written `tyc-emit/src/printer.rs` and only
swap the parser. Less work in this step but pushes the codegen port
into Step 8.

Recommend (a) — it removes the hand-written printer entirely.

### Step 7 — Swap consumer crates, one at a time

Each consumer crate currently does:

```rust
use rustpython_ast::{...};
use rustpython_parser::{parse, Mode};
```

Swap in this order, committing after each:

| # | Crate | Files to touch | Tests to update |
|---|---|---|---|
| 7.1 | `tyc-syntax` | `src/preprocess.rs`, `src/lib.rs` | 21 |
| 7.2 | `tyc-resolve` | `src/lib.rs` | 29 |
| 7.3 | `tyc-types` | `src/lib.rs` | 43 |
| 7.4 | `tyc-analyse` | `src/lib.rs`, `src/auto_gather.rs` | 38 |
| 7.5 | `tyc-desugar` | `src/lib.rs` | 24 |
| 7.6 | `tyc-emit` | `src/printer.rs`, `src/stub.rs`, `src/stubtest.rs` | 29 |
| 7.7 | `tyc-db` | `src/lib.rs` | 56 |
| 7.8 | `tyc-format` | `src/lib.rs` | 13 |
| 7.9 | `tyc-lsp` | `src/lib.rs` | 14 |
| 7.10 | `tyc` (CLI) | `src/commands/*.rs` | 116 |

Common gotchas:

- **`Mod::FunctionType` is missing.** Delete those arms.
- **`Constant` is now `LiteralExpressionRef`** (or similar — check
  Ruff's current naming). Translation is mechanical but tedious.
- **Range type changes from `TextRange` to `ruff_text_size::TextRange`.**
  Same type but different path; a `pub use` re-export in
  `vendor/ruff_python_ast/src/lib.rs` keeps the old name working.
- **`Identifier::new(s)` may have moved.** Ruff uses `Name::new_static`
  for compile-time constants; runtime identifiers go through a
  different path.
- **AST node names lose the `Stmt`/`Expr` prefix** (e.g.
  `rustpython_ast::StmtAssign` → `ruff_python_ast::Assign`). Use
  `cargo fix` plus a `sed` pass for the bulk of the rename.

Expect each crate swap to take 0.5–2 hours. The integration tests
(116 in `tyc/tests/pipeline.rs`) typically all break together when
the CLI crate flips and recover together once the bulk-rename is done.

### Step 8 — Use `mutability` field instead of preprocessing

This is the prize: now that the parser preserves `val`/`var` in the
AST, delete the val/var stripping from
`tyc-syntax/src/preprocess.rs`. The resolver and type checker read
`mutability` directly from the AST node instead of consulting
`PreprocessResult::stripped`.

Drop:
- `PreprocessResult::stripped` field
- `StrippedKeyword` type
- The `for sk in stripped` loops in `tyc-resolve`

This removes ~200 LOC across the codebase and unlocks column-accurate
`val`/`var`-related diagnostics. The preprocess pass still strips other
Typhon keywords (`comptime`, `lazy`, `model`, `impl`, `extend`,
`interface`, `unsafe`) — those get the same treatment in follow-up
PRs.

### Step 9 — Drop the rustpython-parser dependency

Remove `rustpython-parser` and `rustpython-ast` from
`tyc/Cargo.toml`'s `[workspace.dependencies]` once `cargo tree`
shows no consumer references them. Run `cargo deny check` to confirm
no stale entries in `deny.toml`. Bump the project's `Cargo.lock`.

### Step 10 — Final validation

Run the full quality gate:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo bench -p tyc-syntax preprocess  # confirm no regression
cargo bench -p tyc-db incremental    # confirm no regression
```

The round-trip corpus from Step 4 must still pass. Snapshot the
benchmark output and compare against the pre-swap baseline; the Ruff
parser is faster than rustpython-parser in our experience, so a small
improvement is the expected signal.

---

## Realistic effort

| Phase | Effort |
|---|---|
| Steps 1–4 (vendor + smoke test) | 0.5 day |
| Step 5 (val/var tokens) | 0.5 day |
| Step 6 (codegen port) | 0.5–1 day |
| Step 7 (10 consumer swaps) | 1.5–2 days |
| Steps 8–10 (cleanup + validation) | 0.5 day |
| **Total** | **3–4 working days** |

The tree will be partially broken from Step 5 to the end of Step 7.
Do this on a dedicated branch.

---

## Why the scaffold exists now

So the workspace topology and crate boundaries are already in place
when the migration runs. Adding the real source is a drop-in
replacement of the `src/` directories rather than a workspace edit
plus dependency-graph reshuffling.

The scaffold crates are compiled by `cargo build --workspace` so the
dependency edge stays healthy, but they export only a
`SCAFFOLD_MARKER` string that tests can assert on to detect a
half-finished swap.
