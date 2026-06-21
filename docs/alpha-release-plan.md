# Typhon — Full Alpha (v1.0-alpha) Release Plan

> **Status:** Active plan. Authored 2026-06-21 against **v0.15.7**.
> **Target bar:** *feature-complete* alpha — the proven surface **plus** the
> deferred type-system frontier (HKT unification, user-generic variance,
> general inter-procedural field-init, embedded `ty`).
> This document is the execution source of truth for the alpha. The
> [long-term plan](long-term-plan.md) remains the design source of truth;
> where they disagree about *sequencing*, this file wins.

---

## 1. Definition of done — what must be true to tag `v1.0.0-alpha`

The alpha is releasable when **all** of the following hold:

1. **Zero known codegen defects.** The production path (`tyc build` → CPython
   3.13) emits valid, runnable Python for the entire `examples/` + `examples/apps/`
   + `stress/round-*/repros/` corpus. (Already true at v0.15.7 — must *stay* true.)
2. **No confirmed open bugs** in `stress/` or the friction docs that are
   wrong-rejections or silent-wrong-output. Missed-checks are acceptable for
   alpha only if explicitly documented as known limitations.
3. **Third-party type-checking depth** covers the common typed-dependency
   surface: `Annotated[T, …]`, small non-nullable unions, and typeshed-backed
   checking for pure-extension libraries.
4. **Type-system frontier landed and tested:** HKT unification, user-declared
   generic variance, general inter-procedural field-init audit, `ty` Phase 2
   embedded library — each behind tests and (where risky) an opt-in flag.
5. **`tyc migrate` is robust** on a curated real-package set (no invalid output
   such as `mut else:`; clean `migrate → check → build → run` round-trip).
6. **CI quality gates enforced:** `cargo fmt --check`, `cargo clippy -D warnings`,
   full `cargo test --workspace`, the corpus round-trip tests, and a performance
   regression gate.
7. **Docs are release-grade and accurate:** `roadmap.md` reflects reality, the
   stale v0.5.2 feedback doc is archived, the language reference covers every
   shipped keyword, and a migration/"getting started for alpha" page exists.
8. **A published alpha artifact:** installable via `install.sh` / `install.ps1`
   from a tagged GitHub release, with a CHANGELOG `1.0.0-alpha` section and a
   stability/compat statement.

### Explicit non-goals for the alpha
- Full async *inference* (colour stays explicit by design).
- Accumulator-loop parallelisation (comprehension parallelisation already ships).
- Performance parity with hand-written CPython hot paths beyond the current baseline.
- A stability *guarantee* on surface syntax — alpha may still break syntax with
  a documented migration note.

---

## 2. Workstreams

Six workstreams. Each item carries: **scope**, **primary crate(s)/files**,
**acceptance criteria (AC)**, rough **effort** (S ≤ 1d, M ≤ 1wk, L ≤ 2–3wk),
and **dependencies**.

### WS-A — Third-party type-checking depth  *(highest user-visible value)*

| ID | Item | Crate(s) | Effort | AC |
|----|------|----------|--------|----|
| A1 | **Unwrap `Annotated[T, …] → T`** in `annotation_to_type`. Catches wrong-typed kwargs to FastAPI/Typer/Pydantic. Guard against metadata that changes the effective type (validators) by unwrapping to the *first* type arg only and degrading to `Unknown` if ambiguous. | `tyc-types` (`annotation_to_type`), `tyc-venv` | M | New regression tests in `tyc-types` + `tyc-venv`; `typer.Typer(name=123)`-shape is now caught; zero regressions on `examples/` corpus. |
| A2 | **Model small non-nullable unions** (`Union[str, bytes]`, `str \| os.PathLike`) as real `Type::Union` with sound widening (`int→float` inside a union must not narrow incorrectly). | `tyc-types` | M | An `int` arg to a `Union[str, bytes]` param is rejected; valid members still accepted; corpus clean. |
| A3 | **Typeshed-backed checking for pure-extension libs** (numpy/pandas public API). Wire the typeshed pass referenced in the venv introspection report so `.pyi`-only packages get arg-type checks. Sequence after `ty` Phase 2 (WS-D) where typeshed handling can be shared. | `tyc-venv`, `tyc-types` | L | A curated numpy/pandas call with a wrong-typed arg is caught; introspection-miss warning no longer fires for these. |

### WS-B — Migration & interop robustness

| ID | Item | Crate(s) | Effort | AC |
|----|------|----------|--------|----|
| B1 | **Fix `tyc migrate` emitting invalid `mut else:`** (seen on `typing-extensions`). | `tyc/src/commands/migrate.rs` | S | `typing-extensions` migrates to syntactically valid Typhon; regression fixture added. |
| B2 | **Harden migrate on a curated package set** (`python-dateutil`, `humanize`, `click`, a small pydantic user). Reduce post-migrate type errors to zero-or-documented. | `migrate.rs`, `tyc-types` | L | Each package round-trips `migrate → check → build → run`; residual errors triaged into known-limitation docs. |
| B3 | **`.py`-in-`src` + module-name-shadow guard.** Build-time warning (`tyc::stdlib_shadow`) when a module name collides with a stdlib top-level (`types.ty`, `ast.ty`, `parser.ty`). | `tyc-emit`/`tyc` build cmd | S | Building a project with `src/types.ty` emits the warning; clean projects unaffected. |

### WS-C — Type-system frontier  *(the feature-complete differentiator)*

| ID | Item | Crate(s) | Effort | AC |
|----|------|----------|--------|----|
| C1 | **HKT unification.** Wire `Type::TypeConstructor { name, arity }` through `bind_typevars_and_substitute` so `F[A]` binds against `list[int]` (`F=list, A=int`), with kind checking on application. | `tyc-types` | L | `class Functor[F[_]]` + a `map` over `list`/`Option` type-checks; conflicting kinds error clearly; tests cover bind + mismatch. |
| C2 | **User-generic variance inference.** A pass classifies each class type-param's usage (out → covariant, in → contravariant, both → invariant); store per-class; consult in `is_assignable`. Optional explicit `@covariant`/`@contravariant` escape hatch. | `tyc-types` | L | `class Producer[T]` infers covariant `T`; `list[Dog] → Producer[Animal]`-shape assignability works; invariant default preserved where mixed. |
| C3 | **General inter-procedural field-init audit.** Replace the trivial factory-helper special-case with a per-function summary IR tracking partial-instance escapes across calls. | `tyc-types`, `tyc-analyse` | L | A partial instance escaping through a non-trivial helper chain fires `tyc::missing_field_init`; no false positives on the corpus. |
| C4 | **Bounded-typevar variance under interface bounds** (covariant/contravariant flow through `T: SomeInterface`). | `tyc-types` | M | Interface-bounded generic calls honour variance; tests added. |

### WS-D — `ty` integration Phase 2 (embedded library)

| ID | Item | Crate(s) | Effort | AC |
|----|------|----------|--------|----|
| D1 | **Resolve the `cargo deny` git-source blocker** — vendor `ty` under `tyc/vendor/` (path dep) or get an explicit policy carve-out. | workspace, `deny.toml` | M | `cargo deny check` passes with `ty` available as a path/vendored dep. |
| D2 | **Embed `ty` sharing the Salsa db** per `docs/ty-integration.md` Phase 2, eliminating the parse/re-elaborate subprocess round-trip. Keep `--raw`/subprocess fallback. | `tyc`, `tyc-db` | L | `tyc check --with-ty` runs in-process; diagnostics still remap to `.ty`; perf improvement measured vs subprocess. |

### WS-E — DX, LSP & tooling polish

| ID | Item | Crate(s) | Effort | AC |
|----|------|----------|--------|----|
| E1 | **Cross-file go-to-definition** across `.ty`/`.py` via the resolver's cross-module metadata. | `tyc-lsp`, `tyc-resolve` | M | `gotoDefinition` on an imported symbol jumps to the defining `.ty` file; test in `tyc-lsp`. |
| E2 | **AST-based `tyc fmt` reprinter** — bracket-depth-aware spacing around `:`/`=`/`->`; closes the **B15** synthetic-line-number leakage. | `tyc-format` | L | `fmt` is idempotent on the corpus; B15 diagnostics point at real source lines. |
| E3 | **Ergonomics sweep** (opt-in, design-reviewed): same-newtype + newtype/literal arithmetic; `impl` on sealed-union aliases. Land only with a design note each. | `tyc-types`, `tyc-syntax` | M | Each ships behind a test + a `docs/design/` note; no corpus regressions. |

### WS-F — Release engineering, CI & docs

| ID | Item | Crate(s)/area | Effort | AC |
|----|------|---------------|--------|----|
| F1 | **Enforce CI gates:** `cargo fmt --check`, `cargo clippy --workspace -D warnings`, `cargo test --workspace`, corpus round-trip tests. | `.github/workflows` | S | CI fails on fmt/clippy/test/corpus violations. |
| F2 | **Performance regression gate.** Promote `docs/performance-baseline.md` to a CI check that fails on >20% regression of the build pipeline. | `.github/workflows`, bench harness | M | A deliberate slowdown trips the gate. |
| F3 | **Doc refresh:** update `roadmap.md` to v0.15.7→alpha; archive/stamp `examples/apps/TYPHON_FEEDBACK.md` as historical (v0.5.2); ensure the language reference covers every shipped keyword incl. `rescue`. | `docs/`, `examples/` | M | Docs review checklist passes; no doc references a version older than current as "current". |
| F4 | **Alpha cut:** version bump to `1.0.0-alpha`, CHANGELOG section, compat/stability statement, tagged GitHub release, verify `install.sh`/`install.ps1`. | repo root | S | Fresh install from the tag works on Linux + macOS. |

---

## 3. Sequencing & milestones

Three milestones, each independently shippable as a `0.16.x`/`0.17.x`/`0.18.x`
point release, converging on the alpha tag.

### M1 — "Tidy & deepen" (target: 0.16.0) — *2–3 weeks*
The safe, high-value, low-risk batch. Start here.
- **F3** doc refresh, **F1** CI gates  *(do first — fast, unblocks confidence)*
- **B1** `mut else:` migrate fix, **B3** stdlib-shadow guard
- **A1** `Annotated[T, …]` unwrap
- **E1** cross-file go-to-definition
- **Gate:** corpus still 100%, CI green, docs accurate.

### M2 — "Type-system frontier" (target: 0.17.0) — *4–6 weeks*
The feature-complete core. Highest design risk — land behind tests/flags.
- **C1** HKT unification → **C2** variance inference → **C4** bounded variance
- **C3** inter-procedural field-init audit
- **A2** small-union modelling
- **Gate:** new tests pass; zero corpus regressions; each frontier feature has a doc section.

### M3 — "Ecosystem & polish" (target: 0.18.0 → `1.0.0-alpha`) — *3–5 weeks*
- **D1 → D2** embedded `ty`
- **A3** typeshed pure-extension checking (shares D-work)
- **B2** migrate hardening on curated packages
- **E2** AST-based fmt, **E3** ergonomics sweep
- **F2** perf gate
- **F4** cut `1.0.0-alpha`
- **Gate:** §1 Definition-of-done fully satisfied.

> Total indicative window: **~10–14 weeks** of focused work (one person + AI).
> M1 items are parallel-safe; M2 frontier items are mostly serial (C2 depends on
> C1's unifier; A2 benefits from C-work). D2 depends on D1.

---

## 4. Release gates (applied at every milestone tag)

1. `cargo test --workspace` green.
2. `cargo fmt --check` + `cargo clippy --workspace -D warnings` clean.
3. Corpus round-trip tests pass (`corpus_examples_all_check_clean`,
   `third_party_corpus_round_trips_cleanly`).
4. A fresh stress round (`stress/round-<date>/`) over a refreshed 130+ program
   corpus: **0 buildfail / 0 runfail / 0 checkfail**.
5. No new wrong-rejection or silent-wrong-output finding left unresolved.
6. CHANGELOG updated; docs touched by the milestone updated in the same change.

---

## 5. Risk register

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| HKT/variance (C1/C2) introduce assignability regressions | Med | High | Land behind extensive tests; run full corpus as a gate; keep changes *relaxing-only* where possible. |
| Embedded `ty` (D2) blocked by supply-chain policy | Med | Med | D1 explicitly de-risks via vendoring; subprocess Phase 1 remains the shipped fallback — D2 is perf-only. |
| Small-union modelling (A2) causes false positives | Med | Med | Conservative widening rules; start with 2-member unions only; corpus gate. |
| Migrate hardening (B2) reveals deep gaps | Med | Med | Triage residuals into documented known-limitations rather than blocking the alpha. |
| Scope creep delays the tag | High | Med | M1/M2/M3 are independently shippable; the alpha can cut after M2 with M3 items deferred to beta if needed (re-confirm with owner). |

---

## 6. Tracking

Progress is tracked by checking items off §2 and stamping milestone tags. Each
completed item lands with: code + tests + a CHANGELOG line + any doc update, in
one reviewable change. This file is updated as the single live status surface.
