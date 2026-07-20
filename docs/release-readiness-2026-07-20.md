# Typhon v1.0.0-alpha.5 — Release-Readiness Review (2026-07-20)

**Date:** 2026-07-20
**Reviewed tree:** branch `claude/language-release-review-0tcm9q` — `main` (post-alpha.5,
including the three 2026-07-20 patches #321/#322/#325) **plus** the ten pending Dependabot
bumps (#299–#310), which this branch validates and remediates.
**Prior review:** [`RELEASE_READINESS_REVIEW.md`](../RELEASE_READINESS_REVIEW.md)
(v1.0.0-alpha.2, 2026-07-01) — used as the regression baseline throughout.
**Method:** every CI gate run against a fresh release build; the full `examples/` +
`stress/` corpus type-checked; an end-to-end smoke test (init → check → VM run → build →
CPython exec) with VM/CPython output diffing across 18 examples; the perf gate; targeted
adversarial probes re-testing every HIGH finding from the prior review; three parallel
audits (diagnostics catalog, docs/version consistency, release engineering & supply
chain); and upstream verification of every GitHub Action pin touched by Dependabot.

---

## TL;DR verdict

**Ready to release — after this branch merges.** The language, compiler, VM, docs, and
release infrastructure are in materially better shape than at the alpha.2 review, and
every gate is green on this branch. But the review found — and this branch **fixes** —
two genuine blockers that made the pre-review tree unreleasable:

1. **`main` is currently red on the `cargo fmt` CI gate** (commit #322 landed unformatted
   code), so the auto-tag → release pipeline is already blocked on `main` today.
2. **The pending `toml` 0.8 → 1.1 crate bump silently disables third-party
   type-checking's dependency allow-list** (a `tyc-venv` regression test catches it —
   `cargo test` fails on the raw branch). Merging the Dependabot PR without the one-line
   fix in this branch would have shipped the breakage.

Also fixed here: an unverified cross-major artifact pairing in the release workflow, the
regressed `.Jules`/`.jules` case-collision (breaks fresh checkouts on macOS/Windows), an
empty CHANGELOG for a behaviour-changing patch, and a set of catalog/docs staleness gaps.
Everything else — including every HIGH from the prior review — verified clean.

---

## Gate results (this branch, release build)

| Gate | Result | Detail |
|---|---|---|
| `cargo build --release` | ✅ | 2m 12s clean (validates the toml 1.1.3 major bump compiles) |
| `cargo fmt -- --check` | ✅ (after fix) | ❌ on the pre-review tree — #322's tables were unformatted; **CI is red on `main`** until this merges |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | ✅ | zero warnings |
| `cargo test --workspace` | ✅ **2629 passed / 0 failed** (after fix) | ❌ on the raw branch: `tyc-venv::allowed_top_level_reads_declared_dependencies` (toml 1.x break, see F2). Was 2443 tests at the alpha.2 review |
| Example corpus | ✅ **49/49 clean** | 16 projects (incl. all 15 `apps/`) + 33 loose files, zero diagnostics at error severity |
| Stress corpus | ✅ **1083 files, exactly 132 failures** | all 132 are intentional negative fixtures; count identical to the prior review's baseline; zero unexpected passes/failures (the 23 "must_fail-but-passing" third-party fixtures require an installed venv by design — harness-documented) |
| Perf gate (`scripts/perf-gate.sh`) | ✅ | median **47 ms** vs 89 ms baseline (−47%, limit +20%) |
| Smoke test (init → check → run → build → exec) | ✅ | scaffold checks, runs (VM), builds, executes; VM and CPython outputs byte-identical |
| VM ↔ CPython parity sweep | ✅ 13/18 byte-identical | remaining 5 explained below (2 documented VM limits, 2 exception-message-text divergences, 1 missing-pydantic env issue) — no silent-wrong output |
| Diagnostics catalog | ✅ 87/87 | every emitted code has a `docs/diagnostics/` page **and** a `tyc explain` entry; all `url()` deep-links resolve; zero orphans (the 2 extra pages, `freeze`/`pub`, are intentional topic pages) |
| Licensing | ✅ | root MIT `LICENSE`, vendored Ruff notice, vscode `LICENSE`; release archives copy both unconditionally |
| Installers | ✅ | resolve latest incl. pre-releases, fail-closed SHA-256 verification, target triples match the release matrix |
| Supply chain | ✅ (after verification + fix) | all `uses:` SHA-pinned; **every Dependabot-bumped pin verified against upstream tags** (see F3); `deny.toml` strict (no git deps, no copyleft, no muted advisories) |

### Prior-review HIGH findings — all re-verified closed

| Prior finding | Probe | Result |
|---|---|---|
| H1 VM cyclic-value SIGABRT | `a.append(a); b.append(b); a == b` under `tyc run` | ✅ clean exit (see R4 for a residual LOW) |
| H3 checker blowup on nested generics | depth-30 `list[list[…]]` annotation; `match` over `tuple[bool × 22]` | ✅ 12 ms / 9 ms (was >20 s) |
| H5 scope-blind class unification | closed in alpha.4 (evidence-gated guard) | ✅ 3 residual shapes remain documented open in `TYPE_SYSTEM_FRONTIER.md` / prior review addendum |
| H7/H8, MEDIUM VM parity batch | exercised via the 18-example parity sweep | ✅ no silent-wrong output observed |
| Hostile inputs (truncation, unclosed strings, random bytes, invalid UTF-8) | 4 probes | ✅ clean diagnostics, zero panics |
| Alpha.5 features | `tyc explain` for all 9 new lints; `tyc build -O`; `[python] target = "3.15"` → native `lazy import M as A` lowering; invalid target rejection | ✅ all behave as documented |

---

## Findings fixed on this branch

| # | Severity | Finding | Fix |
|---|---|---|---|
| F1 | **BLOCKER (CI)** | #322 landed rustfmt-unformatted keyword tables in `tyc-analyse` and `commands/build.rs` — `cargo fmt --check` fails, so **CI on `main` is red** and auto-tag can't fire. (The patch itself is sound: the six new keywords are prepended ahead of their shorter substrings, preserving alpha.4's longest-first matching, with tests.) | `cargo fmt` applied; gate green |
| F2 | **BLOCKER (pending PR)** | Dependabot's `toml` 0.8.23 → 1.1.3 major bump breaks `tyc-venv::allowed_top_level_from_project`: under toml 1.x, `str::parse::<toml::Value>()` parses a single TOML *value*, not a document, so the reader silently returned an **empty dependency allow-list** — venv introspection (third-party arg/type checking, LSP squiggles) would silently no-op. Compiles clean; only the crate's regression test catches it. | one-line switch to the serde document path (`toml::from_str`), matching the already-correct `tyc-lsp` usage; test passes; the other three `toml::` consumers (`config.rs`, `init.rs`, `tyc-lsp`) verified on compatible APIs |
| F3 | **HIGH (release pipeline risk)** | Dependabot bumped `download-artifact` 4.3.0 → **8.0.1** while `release.yml` still uploads with `upload-artifact@v4.6.2` — an unverified cross-major artifact pairing in the one workflow that publishes user-facing binaries + the `SHA256SUMS` the installers verify, and it can only be exercised by pushing a tag. (All five bumped action pins were **verified against upstream**: each claimed version exists and its tag SHA matches the pin exactly — checkout v7.0.0, setup-node v7.0.0, upload-pages-artifact v5.0.0, deploy-pages v5.0.0, download-artifact v8.0.1. No supply-chain compromise; the earlier in-review suspicion was reviewer-knowledge staleness.) | `download-artifact` re-pinned to the proven `v4.3.0` (SHA verified = upstream tag) with a comment; follow-up recommended in R1 |
| F4 | **MEDIUM (regression)** | `.Jules/` is back beside `.jules/` (both tracked, overlapping filenames) — the case-collision the alpha.3 pass removed; breaks `git checkout` on case-insensitive filesystems (macOS/Windows), i.e. two of the five shipped platforms can't cleanly clone the repo. Reintroduced by automation (#322/#325 wrote to both spellings). | content merged into lowercase `.jules/` (no history lost), `.Jules/` removed, `.gitignore` guard added |
| F5 | MEDIUM | `CHANGELOG.md` "Unreleased" was empty despite #322 changing user-facing behaviour (`tyc::contains_secret_literal` now fires on six more keyword shapes) — violates the repo's "behaviour change ⇒ changelog entry" rule | Unreleased section written covering #321/#322/#325 and this branch's fixes |
| F6 | LOW | `docs/diagnostics/README.md` indexed only 67 of 89 pages — all nine alpha.5 advice lints (and 11 older codes) missing from the human-readable index (the `tyc explain` catalog itself was complete) | index rebuilt: 87 codes + 2 topic pages, alphabetical |
| F7 | LOW | Bundled skill staleness (both `.claude/skills/typhon/` and the byte-identical embedded copy `tyc install skill` ships): `DIAGNOSTICS.md` claimed exhaustiveness but omitted 4 v0.13.0 codes (`mutable_default_param`, `is_literal_comparison`, `incompatible_override`, `loop_closure_capture`); `REFERENCE.md`/`PITFALLS.md` said "Current release: v1.0.0-alpha.2"; `SKILL.md` cited VS Code extension v0.2.1 (actual 0.2.3) and an "83-code" count | all four files fixed in both trees; trees re-verified byte-identical |
| F8 | LOW | README's previous-release cascade skipped v1.0.0-alpha.3 entirely (while alpha.4's own blurb references it, and roadmap/status pages list it) | alpha.3 entry added between alpha.4 and alpha.2 |

## Open recommendations (not fixed here)

| # | Priority | Recommendation |
|---|---|---|
| R1 | Before next infra change | Bump `upload-artifact` **and** `download-artifact` to the same current major together, and exercise `release.yml` end-to-end with a pre-release dry-run tag before the next real release. (This branch parks download on the proven v4 line; Dependabot will re-propose v8.) |
| R2 | Next docs pass | Document two VM ↔ CPython divergence classes in `docs/vm.md` (and the docs-site mirror): **(a) exception message text** — the VM's `json.loads` / `int()` / `as!`-`TypeError` messages differ from CPython's (`"expected null"` vs `"Expecting value: line 1 column 1 (char 0)"`; `invalid literal for int(): "x"` vs `invalid literal for int() with base 10: 'x'`), so programs that *print* caught-exception text — including the shipped `59-boundary-casts` and `60-rescue-boundaries` examples — produce different output under `tyc run` vs `tyc build && python`; **(b) cyclic-value comparison** — `a == b` on self-referential lists returns `False` in the VM where CPython raises `RecursionError` (the alpha.3 fix removed the abort; the residual is benign but is a documentable divergence). |
| R3 | Nice-to-have | `examples/README.md` could note that `57-iterators-generators` and `58-context-managers` need `tyc build` (unbounded generators / `@contextmanager` are documented VM limits with clean error messages — the README's general guidance already steers to `tyc build`, but a per-example note would save a first-run surprise). |
| R4 | Tracked, unchanged | The two deliberately-deferred LOWs from the prior review remain as-is and re-verified: UTF-8 BOM yields a parse error rather than being stripped; `tyc::comptime` errors still render "(no location)" (warnings are correctly located). The three residual H5 collision shapes stay tracked for the origin-threading refactor. |
| R5 | Housekeeping | `.jules/sentinel.md` contains unexpanded `$(date +%Y-%m-%d)` placeholders from its generating script; the Jules automation should also be pointed at the lowercase path only (the `.gitignore` guard added here protects local checkouts, not bot-authored PRs). #325's CSS-only docs-site change was not build-verified in this review (no `npm install` here); the deploy workflow will exercise it on merge. |

---

## What is genuinely in great shape (re-confirmed)

- **The alpha.2 → alpha.5 remediation arc is real.** Every HIGH from the 2026-07-01
  review is closed and stays closed under re-probing; the two deferred LOWs are the only
  survivors, and they remain LOW.
- **Corpus discipline held through three releases**: byte-stable 132 intentional
  negatives, 100% clean examples (now 49 check units), zero compiler panics across the
  corpus and hostile probes.
- **The release machinery is coherent end-to-end**: SHA-pinned actions (now verified
  against upstream), CI-gated auto-tag, `prerelease` derived from the tag, checksummed
  installers that prefer pre-releases, strict `cargo-deny`, licenses in every archive.
- **The catalog invariant holds**: 87 codes ⇔ 87 doc pages ⇔ 87 explain entries, with
  an in-tree test pinning the explain catalog to its listing.
- **alpha.5's headline features behave as documented** — including the nicest
  small thing in this review: `[python] target = "3.15"` really does lower
  `lazy import js = json` to CPython's native `lazy import json as js`.

## Suggested next steps

1. Merge this branch (restores green CI on `main`; carries the Dependabot bumps safely).
2. R1's paired artifact-action bump + dry-run pre-release tag.
3. Fold R2's two VM-divergence notes into `docs/vm.md` + docs-site.
4. Tag the next release from a green `main`.
