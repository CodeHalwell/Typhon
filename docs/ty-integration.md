# `ty` integration plan

Should Typhon integrate with [Astral's `ty`](https://github.com/astral-sh/ty),
the Rust-based Python type checker? **Yes — as a complementary second-stage
checker over the desugared Python, not as a replacement for `tyc-types`.**

This document captures the integration options considered, the recommendation,
and a concrete migration plan.

The [long-term plan](long-term-plan.md) already foreshadows this split
([line 95](long-term-plan.md#L95), [line 350](long-term-plan.md#L350)):
> Custom on Salsa, `ty` as reference / Embed `ty` as a library on the
> desugared AST / Typhon-specific rules (non-null, Result, sealed unions)
> require own checker; `ty` handles the Python subset.

The plan below makes that concrete.

---

## Why integrate

`tyc-types` knows Typhon-specific semantics that `ty` will never know:
non-nullable-by-default, sealed unions with exhaustive `match`,
`Result[T, E]` + `?`, let/mut enforcement, structural conformance for
`interface`, the six-condition `@pure` rule, comptime, and the auto-gather
purity check.

`ty` knows everything else in the Python typing spec — variance,
overloads, generic bounds, the long tail of PEP-695 / 612 / 646 / 692
edge cases, descriptor protocol shenanigans, `typing.Annotated`
metadata. Re-implementing that in `tyc-types` would mean shadowing
years of Astral's work for no Typhon-specific gain.

A two-checker pipeline gets the best of both: Typhon's strict rules
catch the issues we care about most, then `ty` runs as a "second
opinion" against the desugared Python and flags everything else.

---

## What `ty` is, briefly

- Astral's incremental type checker for Python, written in Rust on
  Salsa, sharing parser and AST with Ruff.
- Currently alpha (as of this writing); semantics still settling, so
  treat the integration as opt-in until `ty` ≥ 1.0.
- Available as a CLI today (`ty check path/to/file.py`) and as a Rust
  library (`ty_python_semantic`, `ty_project`) intended to be embedded.
- Same AST shape as the Ruff parser — meaning the [Ruff vendor work](../tyc/vendor/README.md)
  is a prerequisite. Once we're on Ruff, handing the parsed AST to
  `ty` is a function call, not a re-parse.

---

## Integration options considered

### A — Embedded library, runs over desugared Python (recommended)

After `tyc build`'s desugar pass produces a `ruff_python_ast::Mod`
ready for emission, hand it to `ty_python_semantic` and merge its
diagnostics with `tyc-types`'s. Output is a unified diagnostic stream
the user sees as if from a single checker.

```rust
let desugared = desugar_module_with(&module, opts);

// Existing path: Typhon-specific semantics (let/mut, sealed unions,
// Result, interface conformance, purity).
let tyc_diags = tyc_types::check(&desugared);

// New path: standard Python typing spec via ty.
let ty_diags = if config.checker.enable_ty {
    ty_python_semantic::check_module(&desugared.module)
} else {
    Diagnostics::new()
};

all_diags.extend(tyc_diags);
all_diags.extend(ty_diags.map(map_ty_to_tyc_error));
```

**Pros:** No round-trip through disk, no Python interop tax, deepest
information (we feed `ty` the typed desugared form not the original
source), shares the Salsa db with `tyc-types` for incremental
recompilation. Architecturally clean.

**Cons:** Tracks `ty`'s ABI — every `ty` release potentially breaks
the embedding. Requires the Ruff vendor work to land first because
`ty` consumes `ruff_python_ast`, not `rustpython_ast`.

### B — Subprocess `ty check` on emitted `.py`

Run `tyc build` to produce `.py` files, then invoke `ty check build/`
as a separate process; parse its JSON output and re-attribute
diagnostics back to the original `.ty` source via the `.py.map` source
maps.

**Pros:** Zero Rust integration work. Doesn't depend on the Ruff
vendor. Users can swap `ty` for `mypy` or `pyright` trivially.

**Cons:** Slow (re-parses + re-elaborates the entire build on every
check). Each external checker run shells out to Python (a `ty`
process is fast but not free). The accurate diagnostic attribution
this needs is already available — `.py.map` v2 ships the
per-statement `(out_line → ty_line)` table the printer emits.

### C — `ty` as the only checker

Drop `tyc-types` entirely; let `ty` handle everything by encoding
Typhon's strict rules as `Annotated[...]` metadata or custom rule
plugins.

**Pros:** Smallest codebase.

**Cons:** `ty` has no notion of `let`/`mut`, sealed unions, Result
purity, or interface structural conformance. We'd have to fork `ty`
(or wait on plugin APIs that don't exist yet) and have the worst of
both worlds. Rejected.

### D — Cooperative LSPs (live alongside)

Don't integrate at build time at all; let users configure both
language servers in their editor. `tyc lsp` reports Typhon diagnostics
on `.ty` files, `ty server` reports standard-Python diagnostics on
`.py` files.

**Pros:** Zero integration code.

**Cons:** Useless to CLI users; doesn't catch ill-typed Python emitted
by a desugar bug; users would see redundant or conflicting errors when
both checkers fire on overlapping ranges. Useful as a stopgap, not as
a destination.

---

## Recommendation: **A**, with **B** as a transitional fallback

Land option **B** (subprocess `ty check` on emitted Python) first
because it works against today's stable `ty` CLI and doesn't depend
on the Ruff vendor. Use it as an opt-in `tyc check --with-ty` flag
and a `[checker] external = "ty"` config. Most users won't need it
yet, but it gives early adopters something to validate against.

When the Ruff vendor lands (see `tyc/vendor/README.md`), migrate to
option **A**. The subprocess implementation deprecates but stays
behind a config flag for users who pin their own `ty` version.

---

## Step-by-step plan

### Phase 1 — Subprocess `ty check` (option B)

Lands without depending on the Ruff vendor; deliverable today.

#### Step 1.1 — New CLI flag

Add `--with-ty` to `tyc build` and `tyc check`. Off by default. When
set, after the normal build succeeds, shell out:

```bash
ty check --output-format=json build/
```

Capture stdout + stderr; parse the JSON into a `Vec<TyDiagnostic>`.

Implementation: `tyc/src/commands/ty.rs` (new), invoked from
`commands/build.rs` and `commands/check.rs` when the flag is set.

#### Step 1.2 — Config knob

```toml
[checker]
external = "ty"      # or "none" (default) or "mypy" / "pyright" later
external-args = []   # passthrough flags
```

Lives under a new `CheckerConfig` struct in
`tyc/src/config.rs`. Honoured by `--with-ty` and by `tyc lsp` so the
language server runs `ty` on save too.

#### Step 1.3 — Source-map attribution ✅

`ty` reports diagnostics as `path/foo.py:LINE[:COL]: severity: ...`.
`tyc ty` captures the child's stdout/stderr, scans each line for the
`*.py:NN(:NN)?` prefix, looks up the adjacent `.py.map` sidecar, and
rewrites the prefix to `path/foo.ty:TY_LINE[:COL]`. The shared
loader/mapper lives in `tyc/crates/tyc/src/commands/source_map.rs`
and is re-used by `tyc trace`. Pass `--raw` to opt out and forward
`ty`'s output verbatim.

The `.py.map` v2 line table is already shipped, so attribution is at
line granularity from day one. Lines without a recognisable `.py:`
reference (summary text, blank lines, snippet excerpts) are forwarded
unchanged.

#### Step 1.4 — Diagnostic merging

Convert each `TyDiagnostic` to a `TycError::ExternalChecker {
checker: "ty", code, message, span }` variant. Render through the
existing miette pipeline so users see one unified diagnostic stream.
Tag external diagnostics with `[ty]` in the rendered output so the
provenance is clear.

#### Step 1.5 — CI integration

Add a `with-ty` matrix job to `.github/workflows/ci.yml` that
installs `ty` (via `uv tool install ty`) and re-runs the test corpus
with `--with-ty`. Catches the case where Typhon emits ill-typed
Python — a desugar regression that would otherwise slip through.

#### Step 1.6 — Tests

End-to-end test: `tyc build --with-ty <fixture>` on a fixture project
where the emitted Python has a deliberate type error from `ty`'s
perspective (e.g. assigning `int` where `str` is annotated post-desugar
because of a desugar bug). The test asserts that the error surfaces
and points back at the originating `.ty` line.

**Estimated effort: 1–1.5 days.**

### Phase 2 — Embedded library (option A) — ✅ IMPLEMENTED (feature-gated)

> **Status:** shipped behind the `embedded-ty` cargo feature (off by
> default). Build `tyc` with `--features embedded-ty` and the
> `[checker] external = "ty"` / `--with-ty` hook runs `ty` in-process
> instead of spawning the CLI. See `crates/tyc-typecheck-ext`.

**This did NOT require the Ruff vendor migration.** The original blocker —
"`ty` consumes `ruff_python_ast`, which would clash with Typhon's vendored
fork" — turned out to be false. Typhon's vendored crates carry a distinct
version (`0.0.0-typhon-vendor`) from `ty`'s upstream `ruff_python_ast`
(`0.0.0`), so cargo keeps **both** in one dependency graph without conflict
(verified empirically before implementing). No fork rename or upstream
migration was needed.

#### How it was actually done

Rather than vendoring `ty_python_semantic` into the tree, the
`tyc-typecheck-ext` crate takes `ty_project` + `ruff_db` as **optional git
dependencies** pinned to a `ruff` revision, gated behind its `embedded`
feature. With the feature off (the default), neither is compiled and the
crate is a stub — the standard `tyc` build pulls zero `ty`/`ruff` deps.

The check itself mirrors `ty`'s own CLI: `OsSystem::new(dir)` →
`ProjectMetadata::discover` → `ProjectDatabase::fallible(...)` →
`db.check() -> Vec<Diagnostic>`, rendered via `DisplayDiagnostics` in the
CLI's text format so the existing `.py.map` remapper rewrites the diagnostics
to `.ty` source unchanged.

#### Original plan (kept for reference)

The steps below described vendoring `ty_python_semantic` directly. The
optional-git-dependency approach above supersedes them, but they remain a
valid alternative if a fully in-tree vendor is ever preferred.

Pin the upstream `ty` revision in `vendor/UPSTREAM` next to the
pinned Ruff revision.

#### Step 2.2 — New crate `tyc-typecheck-ext`

Don't dump `ty` integration into `tyc-types` directly — keep the
boundary clean. `tyc-typecheck-ext` depends on both `tyc-types` and
`ty_python_semantic` and exposes:

```rust
pub fn check_extended(
    db: &mut TycDatabase,
    module: &Mod,
    desugared: &Mod,
) -> Diagnostics {
    let mut diags = tyc_types::check(db, module);
    diags.extend(ty_to_tyc(ty_python_semantic::check(desugared)));
    diags
}
```

The extension hook means callers (CLI, LSP) don't need to know
whether `ty` is wired up — they call `tyc_typecheck_ext::check_extended`
and the right thing happens.

#### Step 2.3 — Share the Salsa db

`ty_python_semantic` uses Salsa too. Wire its db into Typhon's
`TycDatabase` so a single change-set invalidates both checkers'
caches together. Without this, every `did_change` in the LSP runs
two separate incremental engines that don't share work.

```rust
#[salsa::db]
pub struct TycDatabase {
    storage: salsa::Storage<Self>,
    // ty's queries become accessible via the same db handle.
}

impl ty_python_semantic::Db for TycDatabase { /* ... */ }
```

#### Step 2.4 — Map `ty` diagnostics

Same as Step 1.4 but in-process. Build a `TyDiagnostic → TycError`
translator that preserves `ty`'s diagnostic codes (so users can
suppress specific rules via `# tyc: allow(ty/possibly-unbound)`).

#### Step 2.5 — Deprecate the subprocess path

`--with-ty` keeps working, but defaults to the embedded library when
the user hasn't pinned a `ty` version. Subprocess invocation
remains for two cases: (a) the user wants a version of `ty` newer
than what we vendored, (b) sandbox/policy reasons forbid linking
external Rust code.

#### Step 2.6 — Tests + benchmarks

Comparative benchmark: same fixture project, measured under
`--with-ty=subprocess` vs `--with-ty=embedded`. Expect 5–10× speedup
from skipping the parse + serialize round trip and from shared
Salsa caching.

**Estimated effort: 3–5 days, after the Ruff vendor.**

---

## Out of scope (for now)

- **`ty` as the *only* checker.** Option C in the trade-off table.
  Rejected — Typhon's semantics aren't expressible in `ty`'s rule
  vocabulary.
- **Custom `ty` rules for Typhon idioms.** When `ty` ships plugin
  APIs (likely post-1.0), we can offer `tyc` users a `ty` plugin
  that teaches `ty` about `Result[T, E]` so it can check the
  emitted Python with full fidelity. Until plugin APIs exist, we
  hide Result behind `typhon_runtime` types and `ty` treats them
  as opaque generics — good enough.
- **Replacing the LSP's diagnostic pipeline.** `tyc lsp` will keep
  publishing its own diagnostics; the `ty` integration just feeds
  more diagnostics into the same channel.

---

## Risks

| Risk | Mitigation |
|---|---|
| `ty`'s API is unstable pre-1.0 | Pin a specific commit; treat each upgrade as a deliberate sync (same model as the Ruff vendor) |
| Diagnostic duplication when both checkers fire on the same issue | Code de-duplication step in `map_ty_to_tyc_error` — drop any `ty` diagnostic whose range and category match a `tyc` diagnostic already in the bag |
| `ty` flags Typhon-generated names (`__typhon_gather_…`) as suspicious | Filter on the `__typhon_` prefix in the translator; those are internal and not user-facing |
| Build time regression | Gate behind `--with-ty` / `[checker] external = "ty"` until embedded option proves competitive |

---

## Decision

**Both phases shipped (v0.12.0).** Phase 1 (subprocess `ty check`) is the
default path, exposed via `[checker] external = "ty"` and `--with-ty`.
Phase 2 (embedded, in-process `ty`) shipped behind the `embedded-ty` cargo
feature in `crates/tyc-typecheck-ext` — and notably did **not** require the
Ruff-vendor migration this doc originally assumed (the vendored fork and
upstream `ruff_python_ast` coexist by version). The embedded path is opt-in
at build time so the default binary stays lean; when compiled in, it's
preferred over the subprocess automatically.
