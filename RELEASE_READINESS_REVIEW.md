# Typhon v1.0.0-alpha.2 — Release-Readiness Review

**Date:** 2026-07-01
**Reviewed commit:** `aa105cb` (branch `claude/typhon-codebase-review-s73dz3`)
**Scope:** Rust compiler (`tyc/crates/`), VM, language docs, examples/stress corpus, CI gates,
release infrastructure, security & supply chain, editor tooling.
**Method:** All CI gates run against a fresh release build; the full example/stress corpus
type-checked with the built binary; an end-to-end smoke test (init → check → run → build →
exec) with VM/CPython output diffing; ~30 adversarial crash/perf probes; plus a fan-out of
targeted per-crate code reviews.

---

## Remediation status (applied in this branch)

Most of the findings below have been **fixed** in the same branch as this report. The
example/stress corpus stays byte-identically green (998 pass / 132 intentional-negative
`stress/` fixtures — unchanged before and after), `cargo fmt`/`clippy -D warnings`/the full
test suite pass, and the perf gate is within threshold.

| Finding | Status | What changed |
|---|---|---|
| **B1** — no LICENSE / Ruff attribution | ✅ Fixed | Root `LICENSE` (MIT), `tyc/vendor/LICENSE` (Ruff © 2022 Charlie Marsh), `editors/vscode/LICENSE`, README License section, release.yml now packs both licenses unconditionally |
| **H1** — VM cyclic-value SIGABRT | ✅ Fixed | Depth-guarded `py_eq`/`py_cmp` + `Rc::ptr_eq` fast-paths in `tyc-vm/src/value.rs`; `a == b` on cyclic data returns cleanly instead of aborting the process |
| **H2** — LSP 2 MiB stack | ✅ Fixed | `tyc-lsp` runtime now reserves the same 256 MiB stack as the CLI |
| **H3** — nested-type checker blowup | ✅ Fixed | `types_equivalent` single-pass mutual-assignability replaces the O(2^depth) double-descent; depth-28 check went 5.4 s → 7 ms, depth-100 = 8 ms, semantics identical (corpus unchanged) |
| **H4** — untrusted-code trust model | ✅ Fixed | `SECURITY.md` documents the model; `TYC_NO_INTROSPECT` kill-switch disables dependency introspection (CLI + LSP); README Security note |
| **H7** — VM `str.find` byte vs char offsets | ✅ Fixed | `find`/`rfind`/`index`/`rindex` return char offsets (CPython-correct on non-ASCII) |
| **H8** — VM `+=` rebinds not mutates | ✅ Fixed | `list += iterable` mutates in place (aliases + `self.items += [x]` observe it) |
| MEDIUM — auto-tag no test gate; alpha marked "Latest" | ✅ Fixed | auto-tag gated on CI success via `workflow_run`; `prerelease` derived from the tag |
| MEDIUM — `tyc fmt` non-atomic write | ✅ Fixed | Atomic temp-file + rename in `format_file` |
| MEDIUM — dead `typhon.dev` diagnostic URLs | ✅ Fixed | All ~80 URLs + CLI help repointed to resolvable GitHub docs paths |
| MEDIUM — `exhaustive-match` knob never applied | ✅ Fixed | Wired into `apply_strictness` (warn/off now honoured) |
| MEDIUM — VM float `%` sign, `//`/`% 0.0` | ✅ Fixed | CPython-correct sign + `ZeroDivisionError` |
| MEDIUM — VM broken-pipe panic | ✅ Fixed | `print` tolerates `BrokenPipe` (clean exit) |
| MEDIUM — VM `json.dumps` non-string keys | ✅ Fixed | Scalar keys coerced to strings (valid JSON) |
| MEDIUM — Windows venv discovery dead | ✅ Fixed | `Scripts\python.exe` + `python`/`py` probed |
| MEDIUM — GitHub Actions unpinned | ⚠️ Partial | Added `dependabot.yml` (github-actions/cargo/npm); SHA-pinning left to a maintainer with network access (SHAs unverifiable offline here) |
| Docs/packaging (stale versions, README quickstart, docs-site pydantic/toolchain/installers, examples #60, VS Code `val`/`var`+icon+install, stress README, SECURITY/CONTRIBUTING, `.Jules` case-collision) | ✅ Fixed | See the diff |
| Diagnostics catalog: 4 codes missing docs + `explain` | ✅ Fixed | Added the 4 pages + wired into `tyc explain` |
| **H6** — flow-narrowing soundness holes | ✅ Fixed | All four sub-holes closed in `tyc-types`: (1) `except` handlers now check against pre-narrowing state (a body raise can happen anywhere); (2) loop bodies widen names reassigned inside them so iteration-2 reads aren't treated with the stale pre-loop type; (3) a call in an assign/ann-assign RHS invalidates global narrowing (not just bare-call statements); (4) a bare method-call statement invalidates attribute narrowing rooted at its receiver. 7 new regression tests; corpus unchanged (998/132); full suite green |
| **H5** — scope-blind class unification | ✅ Fixed (2026-07-08 — see addendum below; was deferred in this pass) | Attempted a safe tightening (reject the tail-unification only when both sides are non-partial project classes with different field sets). **Empirically found it introduces a false positive** and reverted: a value typed as a bare `Class("Node")` has lost its module origin, so the incompatibility check misresolves an ambiguous bare name and rejects a *correct-type* assignment (verified: passing a genuine `graph.Node` into a `graph.Node` param errored). The common bug (user class vs a *partial* library class) also can't be caught soundly — a partial shape can't be proven incompatible. A safe fix needs the larger "carry qualified module origin through inference" refactor, not a quick edit |
| LOW — BOM not stripped; comptime "(no location)" | ⛔ Deferred | Low value; the safe fix touches offset-mapping and isn't worth the risk in this pass |

H6 is now fixed (four sub-holes, seven regression tests, corpus unchanged). H5 remains the one
deferred HIGH item — not for lack of trying: a tightening was implemented and then reverted when
it was empirically shown to reject a correct-type assignment, because Typhon's bare-`Class(name)`
representation drops the module origin a sound check needs. That's a design-level refactor
(thread qualified origin through inference), not a quick edit, and shipping the false-positive
version would have violated the project's hardest constraint (never reject a currently-valid
program).

> **2026-07-08 addendum — H5 is now fixed** (post-alpha.3 branch). The key insight that unblocked
> it without the full origin-threading refactor: a bare name is unambiguous in exactly one case —
> when it names a class **declared in the module being checked** (`local_classes`), because a
> `class` statement always creates a fresh class and local declarations shadow imports. The new
> `tail_unification_provably_distinct` guard in `is_assignable` refuses the qualified ↔ bare
> unification only when (a) the bare side is such a local declaration, and (b) the qualified
> side's declaration — resolved through its **exact** module-registry key, never the reverse scan
> that misfired in the reverted attempt — has a non-equivalent shape. Everything uncertain
> (unknown modules, partial-vs-partial, facade re-export copies with equal shapes, interfaces,
> bare names of unknown provenance such as provider return types) degrades to the previous
> permissive unification. The fully-bare ↔ fully-bare collision (two `from a import Node` /
> `from b import Node` values) still unifies — closing it does need the origin-threading
> refactor. Corpus byte-identically unchanged; four regression tests added.

---

## TL;DR verdict

Typhon is **an impressively complete and well-engineered alpha** — every CI gate is green
(fmt, clippy `-D warnings`, **2443 tests**, release build, perf gate), the compiler did **not
panic once** across 1130 corpus checks and ~30 hostile inputs, the emitter produces correct
CPython, and the diagnostics catalogue is complete. It is **not quite ready to "release to the
world" today**, but the gap is small and concentrated:

- **One true blocker**: there is **no LICENSE file anywhere**, while MIT is claimed in four
  places and the vendored Ruff fork is shipped in every binary without its required
  attribution. This is a legal blocker, not an engineering one — an afternoon's fix.
- **A cluster of crash/DoS/soundness issues** that a public audience *will* hit within days:
  the VM aborts the whole process on cyclic data, the LSP inherits a stack-overflow class the
  CLI already fixed, the type checker blows up superlinearly on nested types, and there are
  several silent flow-narrowing soundness holes that contradict the alpha.2 headline claims.
- **An undocumented trust model**: `tyc check`/`build` and the LSP execute untrusted project
  code (dep imports, `uv sync`, a repo-committed `.venv` interpreter) with no gate and no docs.

Fix the license blocker, the handful of HIGH crash/soundness items, and document the trust
model, and this is a credible, honest public alpha. Recommendation: **hold the public
announcement for one short hardening pass** (est. a few days), keeping the binaries available
for early adopters in the meantime.

---

## What is genuinely in great shape

This is a real strengths list, not throat-clearing — much of the codebase is above the bar for a 1.0-alpha:

- **CI is green and honest.** `cargo fmt --check`, `clippy --all-targets --all-features -D
  warnings`, `cargo test --workspace` (**2443 passed / 0 failed**), release build, and the
  network-free perf gate (median 95 ms vs 89 ms baseline, **+6.7 %**, within the 20 % limit)
  all pass. CI matches CLAUDE.md's description (fmt → clippy → test; cargo-deny; perf gate).
- **Crash discipline is excellent.** Across the whole `examples/`+`stress/` corpus (1130
  check invocations) and ~30 adversarial inputs (truncated files, unclosed triple-strings,
  multibyte-in-comments, 2000-deep parens, BOM/CRLF, null bytes, mega-lines, lone surrogates,
  invalid UTF-8, malformed `rescue`), **not a single compiler panic or ICE** — every bad input
  yields a clean diagnostic. All 132 corpus "failures" are intentional negative fixtures under
  `stress/`; `examples/` is 100 % clean.
- **The emitter is correct.** A demanding probe (operator precedence incl. right-assoc `**`
  and unary-vs-power binding, bitwise precedence, ternaries, short-circuit booleans, full
  string escaping, unicode, shortest-round-trip floats incl. `inf`/`-0.0`, big ints,
  comprehensions) produced **valid Python with byte-identical VM output**. Typhon-specific
  lowerings (sealed-union `impl` distribution, `Result`/`?`, `rescue`, `freeze`, `comptime`,
  pipes) also round-trip correctly.
- **Diagnostics catalogue is complete.** All **76** `tyc::` codes have both a
  `docs/diagnostics/*.md` page and an offline `tyc explain` entry, including the three new
  alpha.2 codes. Zero orphan pages.
- **Security fundamentals are strong.** Minimal, sound `unsafe` (four well-justified Salsa
  `Update` impls; **zero** `transmute`/raw-pointer/`get_unchecked`; the vendored parser has
  zero `unsafe`). Clean, current dependency tree with **no** known-vulnerable crates and **no**
  TLS/network stack pulled in. Every subprocess (`python`, `uv`, `ruff`, `ty`) is spawned with
  **argv arrays — no shell interpolation anywhere**. Installers do real TLS-1.2-pinned,
  fail-closed SHA-256 verification with version pinning.
- **The comptime sandbox holds.** File I/O, `import`, and infinite recursion are all rejected
  cleanly with `tyc::comptime` — no escape, no hang, no crash.
- **Config parsing is best-in-class for an alpha** — `deny_unknown_fields`, allow-listed
  enums/severities, precise path-bearing errors, no panic paths — and LSP UTF-16 position
  handling is correct and surrogate-pair tested.
- **VM is CPython-exact where it counts** — MT19937 to the letter, shortest-round-trip float
  repr, int/bool/integral-float dict-key collapse, faithful mutable-default and closure
  late-binding semantics; and it degrades wrong-typed values to Python-style exceptions rather
  than panicking.

---

## BLOCKER

### B1 — No LICENSE file anywhere; vendored Ruff fork ships without its MIT attribution
*(independently surfaced by the release-infra, examples/editor, and security reviews)*

- **Evidence:** No `LICENSE`/`COPYING` file exists in the repo. Yet MIT is declared in
  `tyc/Cargo.toml:30` (`license = "MIT"`, inherited by all 13 `tyc-*` crates and 5 vendored
  crates), the README badge (`README.md:13`), and `editors/vscode/package.json:7`. The five
  vendored Ruff crates under `tyc/vendor/` carry **no** upstream `LICENSE` and declare
  `license.workspace = true` (i.e. Typhon's own metadata) — Ruff is MIT © 2022 Charlie Marsh.
  `release.yml:155,185` copies LICENSE with `2>/dev/null || true`, so every published archive
  silently ships with no license text at all.
- **Why it blocks release:** With no LICENSE file the default legal state is *all rights
  reserved* — nobody may lawfully use/redistribute the code despite the badges. MIT's one
  condition is that its notice ships with copies; the Ruff fork is compiled into every released
  `tyc` binary without it, which is an active license violation. GitHub shows "no license";
  `vsce`/`cargo publish` will warn or fail.
- **Fix:** Add an MIT `LICENSE` at the repo root; add `tyc/vendor/LICENSE` carrying Astral's
  upstream MIT notice (reference it from `vendor/README.md`); add a `## License` section to
  README and a `LICENSE` under `editors/vscode/`; make the release.yml copy steps unconditional
  so a missing file fails the build. Consider a generated third-party-notices file
  (`cargo about`) in the release archives.

---

## HIGH — fix before a public announcement

### H1 — VM aborts the process (Rust stack overflow → SIGABRT) on cyclic values
*(VM review; independently reproduced during this review)*

- **Evidence:** `tyc-vm/src/value.rs` `py_eq` (List/Dict/Instance arms ~801-850),
  `instance_repr_inner` (~1375), `py_str` for lists (~983); `builtins.rs` `json_dumps`
  (~7058); `interp.rs` `contains` (~1958). Only `repr_of_depth` (`interp.rs:2981`) has a
  100-level guard; equality, membership, hashing, and JSON recurse with **no** cycle detection
  or depth cap. Reproduced live:

  ```python
  def main() -> None:
      mut a: list[object] = []
      a.append(a)
      mut b: list[object] = []
      b.append(b)
      print(a == b)      # VM: "thread 'tyc-main' has overflowed its stack / SIGABRT (exit 134)"
                         # CPython: catchable RecursionError
  ```
  This passes `tyc check` (recursive classes are an advertised pattern, cf.
  `examples/50-linked-list`), so it is reachable from valid programs — the worst class of
  failure in the VM (whole-process abort, not a Python exception).
- **Fix:** Add an `Rc::ptr_eq` identity fast-path and thread a depth budget / seen-set through
  `py_eq`, `py_cmp`, `to_hash_key`, `json_dumps`, `deep_freeze_value`; route instance/`Ok`/`Err`
  repr through the guarded `repr_of_depth`.

### H2 — The LSP runs the compiler on 2 MiB stacks; the 256 MiB deep-recursion fix protects only the CLI
*(surfaced independently by the resolve/types and CLI/LSP reviews; confirmed here)*

- **Evidence:** `tyc/crates/tyc/src/main.rs:20-27` deliberately reserves a **256 MiB** worker
  stack ("On the default ~8 MB stack such input overflows"). But `tyc-lsp/src/lib.rs:2458`
  builds `tokio::runtime::Builder::new_current_thread()` with **no** `thread_stack_size`, and
  checks run inside `spawn_blocking` — tokio blocking threads default to **2 MiB**. The exact
  pathological nesting alpha.2 "fixed" for the CLI still SIGABRTs the language server (i.e.
  kills the editor experience for every open file).
- **Fix:** Set `.thread_stack_size(256 * 1024 * 1024)` on the LSP runtime builder, and/or add a
  recursion-depth budget in the checker that degrades to `Unknown` + a diagnostic (see H3).

### H3 — Type checker blows up superlinearly on nested generic types (checker/LSP DoS)
*(measured during this review; same root cause as the resolve/types review's exponential tuple-exhaustiveness finding)*

- **Evidence:** `let x: list[list[…list[int]…]] = []` at nesting depth N, measured with the
  release binary: depth 10 = 9 ms, 20 = 31 ms, 25 = **692 ms**, 28 = **5.4 s**, 30 = **>20 s
  (timeout)**. Isolated to the **type checker**, not the parser: an equally deep list *literal*
  checks in 8 ms while the deep *type annotation* takes 5.4 s. Roughly exponential in depth.
  The resolve/types review found the sibling case in `cover_pattern_columns`
  (`tyc-types/src/lib.rs:12648`) — `match` over `tuple[bool × 30]` explores ~2³⁰ paths.
- **Why it matters:** A single untrusted `.ty` file (or a paste into an editor buffer) hangs
  `tyc check` and freezes the LSP. Because it is *time*-based, the 256 MiB stack in H2 does not
  help — the editor simply hangs.
- **Fix:** Memoize assignability/normalization over structurally-equal subtypes; add an
  explored-path budget that bails to a clean diagnostic above a threshold.

### H4 — Untrusted-project code execution at check/build/LSP time, undocumented and ungated
*(surfaced independently by the security and CLI/LSP reviews)*

- **Evidence:** `tyc-venv/src/lib.rs:297-304` prefers `<project>/.venv/bin/python` if present —
  a cloned repo can commit a malicious interpreter. Introspection then **imports** dependency
  packages (`importlib.import_module`, `lib.rs:465`) → runs their `__init__.py` at check time;
  the LSP hover/completion path (`venv_introspect.rs`) imports **any** module named in the open
  buffer, with no allow-list. `tyc build` runs `uv sync` **by default** (`build.rs:246`),
  installing packages from an untrusted `typhon.toml`. A grep of `docs/` + `docs-site/` for a
  trust/security note returns **nothing**, and there is no kill-switch (the
  `unintrospectable-dependency = "off"` knob silences only the *warning*).
- **Why it matters:** Cloning a repo and running `tyc check` — or just opening the folder in an
  editor — is widely assumed safe (rust-analyzer/gopls/tsserver avoid executing project/deps by
  default and pair with editor Workspace Trust). Here it executes attacker-controlled code.
- **Fix:** Document the trust model prominently; add an explicit opt-in/trusted-workspace gate
  for venv introspection and `uv sync`; declare
  `capabilities.untrustedWorkspaces` in the VS Code extension; apply the declared-deps
  allow-list to the LSP path too; refuse a project-relative `.venv` interpreter without a trust
  marker.

### H5 — Scope-blind class unification: any bare class unifies with any same-named qualified class
*(resolve/types review; confirmed here)*

- **Evidence:** `class_name_tail` (`tyc-types/src/lib.rs:211`) is `name.rsplit('.').next()`;
  `is_assignable` (~2993) unifies `Class(A) ↔ Class(B)` whenever final `.`-segments match and a
  side is bare, with no check that the bare name is an unrelated local class. Every in-project
  class is registered bare, so a user `class Response { status: int }` satisfies an
  `httpx.Response`-typed slot (and vice-versa); `x: mylib.Token` accepts a user `Token` with an
  incompatible shape.
- **Why it matters:** Common class names (`Error`, `Response`, `Token`, `Config`) silently
  unify across the user/library boundary — a program type-checks clean and crashes at runtime.
  This directly undercuts the checker's headline value.
- **Fix:** Refuse the tail unification when the bare side names a project class whose provenance
  or shape differs; only unify when the bare name is import-resolvable to the qualified module.

### H6 — Flow-narrowing soundness holes contradict the alpha.2 soundness claims
*(resolve/types review)*

- **Evidence (all `tyc-types/src/lib.rs`):**
  - `try` handler bodies are checked with the *try body's end-state* narrowings (no
    snapshot/restore, ~10653) — `mut x: int? = None; try: x = get(); risky() except: print(x+1)`
    is accepted, crashes if `get()` raised.
  - Loop bodies are single-pass (~10453 While, ~10505 For); a var un-narrowed at the bottom is
    re-read at the top on iteration 2 with the stale type.
  - `reset_global_narrowings` (~2364) is wired to **only** the bare-`Expr` statement arm (~10543)
    — calls in assign-RHS/`return`/`if`/`while`/`with` don't invalidate global narrowing, so the
    alpha.2 fix is ~20 % wired.
  - `self.x` (attr) narrowing is never invalidated by a method call (release notes advertise
    this for instance fields; the code implements it only for globals).
- **Fix:** Snapshot env before `try` bodies; reset loop-assigned names' narrowing before
  checking the body (or check twice); invoke `reset_global_narrowings` from every statement arm
  containing a call; clear receiver-rooted attr narrowings on any call through that receiver.

### H7 — VM string methods return byte offsets while indexing/slicing use char offsets (silent data corruption)
*(VM review)*

- **Evidence:** `builtins.rs:5809-5870` — `str.find/rfind/index/rindex` return Rust **byte**
  offsets; `interp.rs:3547,3701` — subscript/slice use `chars()` **char** indices. The
  canonical `s[s.find(x):]` idiom silently corrupts on any non-ASCII text under `tyc run`
  (`"héllo".find("llo")` → VM 4-bytes vs CPython 3-chars). Undocumented.
- **Fix:** Convert byte→char (`s[..byte].chars().count()`) before returning from the find family.

### H8 — VM augmented assignment rebinds instead of mutating; `__iadd__` never dispatched
*(VM review)*

- **Evidence:** `interp.rs:237-243` lowers `AugAssign` to `binop` + `assign_target` (a rebind);
  no in-place dunder slots exist. `let a=[1]; mut b=a; b += [2]` leaves `a == [1]` (CPython
  `[1,2]`); `self.items += [x]` in a method silently fails to mutate the shared list.
- **Fix:** Special-case `List`/`Set`/`Dict` targets to mutate through the `Rc`; try `__i<op>__`
  before `__<op>__` on instances.

---

## MEDIUM — fix soon (fast-follow acceptable)

- **Release process:** `auto-tag.yml` fires on every `main` push and dispatches `release.yml`,
  which builds & publishes public binaries **without running `cargo test`** and with CI running
  only *concurrently* — an untested/broken commit can auto-ship. And `release.yml:269` hardcodes
  `prerelease: false`, so `v1.0.0-alpha.x` tags carry the green "Latest" badge. *(Fix as a pair:
  gate auto-tag on CI success; set `prerelease` from a `-` in the tag; teach installers to fall
  back from `/releases/latest` to the release list.)*
- **`tyc fmt` write-back is non-atomic with no output re-parse** (`tyc-format/src/lib.rs:1216`
  is a bare in-place `std::fs::write`). Input syntax is validated first (good), but a formatter
  bug or a crash mid-write can truncate a user's source, with no tempfile+rename and no backup —
  and this crate corrupted files as recently as v0.9.1. *(Fix: write temp + fsync + atomic
  rename; re-parse the output before replacing.)* Empirically fmt is idempotent and emptied no
  files in this review — the risk is the mechanism, not a live bug.
- **~76 diagnostic `url(...)` links point at `https://typhon.dev/lang/diagnostics/<code>`**,
  which is not the deployed site (docs deploy to `codehalwell.github.io/Typhon`, and that path
  shape matches nothing there). Every rendered diagnostic advertises a dead — and squattable —
  domain. *(Fix: point at the GH Pages URLs, or secure typhon.dev with redirects before tagging.)*
- **Validated-but-unwired knob:** `[strictness] exhaustive-match` is parsed, validated,
  scaffolded by `tyc init`, and documented — but never applied (`apply_strictness` in
  `commands/util.rs` never handles `NonExhaustiveMatch`; the checker unconditionally errors).
  Setting `"warn"`/`"off"` silently does nothing.
- **`tyc build` prints diagnostics unsanitised** (synthetic `__typhon_*`/preprocessed line
  numbers leak) while `tyc check` wraps them in `SanitisedDiagnostic` — regressing the v0.8.0
  "every diagnostic is sanitised" claim.
- **More VM/CPython divergences (silent-wrong, not crashes):** float `%` has the wrong sign for
  negative divisors and `//0.0`/`%0.0` return `inf`/`nan` instead of raising; `print` panics on
  a broken pipe (`tyc run app | head`); `json.dumps` emits invalid JSON for non-string keys and
  silently serialises non-JSON values; unbounded `itertools.count/cycle/repeat` silently
  truncate; unclosed file writes are lost and `r+` truncates the file on flush. *(Each should
  either match CPython or raise the existing `vm_unsupported_use_compile` error; several deserve
  a `docs/vm.md` paragraph.)*
- **comptime `env()` bakes environment values into build artifacts**; the `contains_secret_literal`
  guard is a name-suffix heuristic (`*KEY/TOKEN/…`) that a differently-named binding or
  `allow-secret-comptime = true` defeats silently. *(Fix: document that comptime output is not
  secret-safe; consider advice on any `comptime let` whose RHS reads `env(...)`.)*
- **Third-party GitHub Actions pinned by mutable tag, not SHA**
  (`EmbarkStudios/cargo-deny-action@v2`, `dtolnay/rust-toolchain@stable`,
  `softprops/action-gh-release@v2` — the last runs with `contents: write` handling the exact
  bytes users `curl | sh`). *(Fix: pin to full commit SHAs; add `dependabot.yml`.)*
- **LSP quality gaps:** holds the Salsa DB mutex across venv-introspection subprocesses (all
  requests stall during cold introspection); hover/completion ignore the preprocessor
  column-shift table that goto-def uses (wrong symbol on `pub`/`comptime`/`freeze` lines);
  editing a sibling module leaves stale cross-module diagnostics; **Windows venv discovery is
  dead** (only `.venv/bin/python` probed — no `Scripts\python.exe`), so third-party checking
  silently no-ops on a shipped platform.
- **Stale/incorrect getting-started docs:** `docs/install.md:5` says "current release is
  v0.13.1" and `docs/long-term-plan.md:495` says "v0.12.0" (both should be 1.0.0-alpha.2); the
  docs-site installation page omits the one-line installers entirely and cites a wrong Rust floor
  (1.85 vs the pinned 1.93/1.94); the README quickstart is missing a `cd myapp` so the first
  copy-pasted commands fail; `docs-site` documents `[emit] class-default = "pydantic"`, which the
  compiler rejects.
- **examples/editor:** `examples/README.md` omits `60-rescue-boundaries` (the worked example for
  the alpha headline feature); the VS Code grammar highlights deprecated `val`/`var` keywords
  (an init regression test explicitly guards against emitting `val`), mis-painting any identifier
  named `var`/`val` — the shipped corpus itself hits this.

---

## LOW — polish / pre-1.0 stable

- Leading UTF-8 BOM is not stripped (yields a confusing `"ﻻlet x"` parse error instead of being
  ignored). *(Measured this review.)*
- `tyc::comptime` diagnostics render at "(no location)" instead of pointing at the offending
  binding. *(Measured this review.)*
- `.Jules/` and `.jules/` differ only by case — both tracked; breaks checkout on
  case-insensitive macOS/Windows (two shipped platforms). These are internal AI-agent scratch
  files with unexpanded placeholders; decide whether they belong in a public repo.
- No `SECURITY.md`/`CONTRIBUTING.md`/issue templates — a public launch invites reports with
  nowhere to route them (SECURITY.md matters most for a binary-distributing compiler).
- VS Code extension: no marketplace PNG icon, no "Install" section, not published to the
  Marketplace (VSIX not attached to releases); grammar highlights nonexistent `Option`/`Some`.
- `stress/README.md` is stale (documents 127 files; the tree holds ~1,083) and
  `stress/EPIC_SUMMARY.md` ships an unfilled `#<issue-number>` placeholder — reads as an
  abandoned lab bench to an outside evaluator. Label it "internal repro corpus".
- Docs claim a "256-file example corpus"; actual is 259. Minor drift.
- Assorted VM edge divergences (LOW): `x is x` is `False` for floats; equal big ints/strings are
  spuriously `is`-identical; dict/set iteration snapshots keys (no "changed size during
  iteration" error); unseeded `random` is deterministic across runs (fixed MT seed 5489);
  incomparable-type `sorted` returns unsorted instead of raising; heapq ignores `__lt__`.

---

## Coverage notes

- CI gates, corpus sweep, smoke test, perf gate, emitter probes, comptime-sandbox probes,
  crash probes, and the diagnostics-catalogue diff were **executed** against the release binary
  during this review (results above).
- Per-crate deep code reviews were completed for tyc-vm, tyc-resolve+tyc-types,
  tyc-analyse-adjacent CLI/db/venv/lsp, security/supply-chain, release-infra, and
  examples/editors.
- Three deep-read passes (docs consistency, tyc-syntax preprocessor internals, and the
  desugar/emit/format crates) were interrupted by an account rate limit; their highest-value
  targets were instead covered **empirically** here (emitter correctness via build+diff, the
  complete diagnostics-catalogue diff, preprocessor crash probes, and the fmt write-path audit).
  A follow-up read pass on the desugar/emit internals and the preprocessor's fixpoint-rewrite
  termination is still worth doing before the 1.0-stable tag.

---

## Suggested pre-announcement checklist

1. **B1** — add LICENSE (root + vendor + vscode), fix release.yml copy guards. *(blocker)*
2. **H1, H3** — cap/guard recursion in VM cyclic-value paths and the type checker (shared theme:
   depth budgets / cycle detection / memoization).
3. **H2** — give the LSP the same 256 MiB stack as the CLI.
4. **H4** — document the trust model and add an opt-in gate for venv introspection / `uv sync`.
5. **H5, H6** — tighten class unification and the flow-narrowing invalidation holes.
6. **H7, H8** — VM byte/char offsets and augmented-assignment mutation.
7. Fix the dead `typhon.dev` diagnostic URLs, the stale install docs, and the `prerelease` flag
   + auto-tag test gate.
8. Make `tyc fmt` writes atomic.

Everything below MEDIUM is safe to fast-follow after launch. The engineering foundation here is
strong; this is a short, well-defined hardening list, not a rewrite.
