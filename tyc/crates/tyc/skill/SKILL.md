---
name: typhon
description: Write, check, build, debug, and migrate code in the Typhon language — a statically-typed, stricter superset of Python that compiles to clean CPython 3.13+ via the `tyc` binary. Use this skill whenever you are editing `.ty` / `.dty` / `typhon.toml` files, invoking any `tyc` subcommand (`build`, `check`, `fmt`, `lsp`, `init`, `run`, `repl`, `debug`, `trace`, `profile`, `explain`, `cheatsheet`, `stubtest`, `migrate`, `ty`, `add`, `remove`, `sync`, `install`), translating Python to Typhon, debugging Typhon-specific diagnostics, working on the Rust compiler crates under `tyc/crates/`, or answering any question about the language, the compiler pipeline, the in-process VM, the generated `typhon_runtime`, or the project's docs. Triggers include: file extensions `.ty` / `.dty` / `.py.map` / `typhon.toml`, the words "Typhon" / "tyc" / "let binding" / "mut binding" / "freeze let" / "newtype" / "pub" / "pub *" / "Result[T, E]" / "sealed union" / "interface" / "impl block" / "extend block" / "gather:" / "go f(x)" / "comptime" / "@gatherable" / "@pure" / "@memo" / "T?" / "plain class" / "class!" / "model class" / "lazy import" / "lazy let" / "unsafe:" / "as!" / "checked cast" / "rescue" / "guard" / "|>" / "typhon_runtime" / "tyc-vm" / "tyc-syntax" / "tyc-resolve" / "tyc-types" / "tyc-analyse" / "tyc-desugar" / "tyc-emit" / "tyc-format" / "tyc-db" / "tyc-diagnostics" / "tyc-lsp", and any error code matching `tyc::...`.
---

# Typhon — Language, Compiler, Runtime, and VM Skill

Typhon is **a statically-typed, stricter superset of Python that emits clean CPython 3.13+** with zero runtime dependency on the toolchain. The compiler, language server, formatter, debugger wrapper, REPL, and tree-walking interpreter are all a single Rust binary called `tyc`. Every `.ty` file emits valid, idiomatic `.py`. Not all `.py` is valid Typhon.

**Current release: v1.0.0-alpha.9** (2026-08-21) — a maintenance release on top of alpha.8, with no language change. The **warn-level** `tyc::contains_secret_literal` keyword table grows from 16 entries to 55: seven overlapping proposals consolidated into one longest-first-ordered table (`AUTHORIZATION`, `CREDENTIALS` / `CREDENTIAL`, `WEBHOOK`, `SIGNING`, `COOKIE`, `DSN`, the `DB_`-prefixed password / pass / pwd / secret variants plus a secret-only `APP_` / `CLIENT_` / `JWT_`, the `ACCESS_` / `AUTH_` / `BEARER_` / `CSRF_` / `JWT_`-prefixed token variants, and `PRIVATE_KEY` / `PUBLIC_KEY` / `SSH_KEY` / `SECRET_KEY`, each with its squashed-acronym form), so `DB_PASSWORD` now reports `DB_PASSWORD` rather than the bare `PASSWORD`. One more name-word boundary lands with it — an uppercase keyword directly followed by a lowercase letter (`TOKENs`, `dbPASSWORDstring`); `MONKEY` still does not match `KEY`. Alongside: three more allocation reductions on compiler AST walks (`auto_gather`'s candidate scan, `module_all_names`, and `tyc build`'s import scan), the mid-August dependency wave (with the `compact_str` 0.10 bump reverted — `get-size2` 0.8 pins 0.9.1, and the duplicate broke the vendored Ruff build), and docs-site keyboard-accessibility / reduced-motion polish. **No new syntax and no new error-level diagnostic.** **v1.0.0-alpha.8** (2026-08-08) was a maintenance release on top of alpha.7. One VM ↔ CPython parity fix: the VM's CPython-compatible MT19937 was seeded from a fixed constant when a program never called `random.seed()`, so an unseeded program drew the *same* sequence on every `tyc run` while `tyc build && python` drew a different one each time (CPython seeds from OS entropy at import). The default now seeds from entropy to match; **explicit `random.seed(n)` is unaffected and still yields byte-identical sequences under both surfaces.** Alongside it: the warn-level `tyc::contains_secret_literal` name lint now treats digits and UPPERCASE→TitleCase junctions as word boundaries (`myPASSWORD123`, `dbPASSWORDString` are flagged; `MONKEY` still does not match `KEY`), two allocation reductions on AST walks (`parallel_lints`, the desugarer's class-marker collection), the early-August dependency wave (five crates, two GitHub Actions majors), and docs-site card / link-card transition polish. **No new syntax and no new error-level diagnostic.** **v1.0.0-alpha.7** (2026-07-29) was the 2026-07-28 full-codebase-review remediation release (`docs/codebase-review-2026-07-28.md`). It closes all ten of the review's 1.0 blockers plus the Tier-0 gates that make them verifiable: type-checker soundness fixes (instance-attribute assignment is now type-checked, constructor field order follows reverse MRO, model constructors are keyword-only, recursive type aliases terminate, parametric sealed-union match exhaustiveness is checked, and a new nullable-field-dereference check lands at warn level), resolver fixes (`let` immutability is now enforced inside loop bodies and through `global`/`nonlocal`), a shared-lexical-mask rewrite of the preprocessor fixing several literal-corrupting bugs (`gather:` dependency scanning, `with`-chain parsing, `enum` body rewriting), five emitter/preprocessor miscompilation fixes, and VM `ExceptionGroup`/`except*` support (PEP 654) plus five other VM↔CPython divergence fixes. Two new CI gates ship alongside: a VM↔CPython differential gate over the full 1342-file example+stress corpus (first run found 126 divergences, now pinned as a baseline), and an opt-in-knob codegen matrix. **No new syntax; most changes are narrowings on code that was already crashing or relying on unsound typing, plus one warn-level diagnostic-surface addition.** **v1.0.0-alpha.6** (2026-07-21) was a maintenance release on top of alpha.5, driven by the 2026-07-20 release-readiness review (`docs/release-readiness-2026-07-20.md`). The July dependency wave is carried safely across the `toml` 0.8 → 1.x major — including the fix, before it could ship, for the one regression that bump introduced (the `tyc-venv` dependency allow-list reader silently returning empty, which would have no-opped third-party arg/type checking). Six squashed-acronym keywords join the secret-name table (`API_TOKEN`, `APITOKEN`, `API_SECRET`, `APISECRET`, `API_PASSWORD`, `APIPASSWORD`), now hoisted into a single shared `tyc_analyse::SECRET_NAME_KEYWORDS` constant so the `tyc::contains_secret_literal` lint and the `tyc build` scan can no longer drift. The release pipeline's artifact download is re-pinned to the proven v4 line (matching the v4 upload), and docs-site accessibility, diagnostics-index completeness (all 87 pages indexed), and repo hygiene (the `.Jules`/`.jules` case-collision fix) round it out. **No new syntax; no previously-*correct* program changes behaviour** — the only diagnostic-surface change is warn-level (six more secret-shaped names warned). **v1.0.0-alpha.5** (2026-07-09) was a performance-focused release on top of alpha.4. **VM performance Tier 1** (`tyc run`): a two-representation `VmInt` (`Small(i64)` inline, `Big(BigInt)` on overflow) keeps common-path integer arithmetic allocation-free while preserving CPython's exact arbitrary-precision semantics; a per-class resolved-method cache (negative results included) and a direct `obj.method(args)` dispatch path skip redundant `find_method` walks and `BoundMethod` allocation; and functions with no closures/`global`/`nonlocal` resolve locals to a fixed slot computed once instead of a `HashMap` lookup per read/write. Cumulative effect: the VM's steady-state slowdown vs `tyc build` + CPython compresses from ~5–18× to ~3–14× (startup-adjusted); the VM still wins on startup latency. **`[optimise]` config profile + `tyc build -O`:** a single `level` dial flips the default of `auto-memoise` / `auto-gather` / `auto-parallel` / `pgo-memoise` to `true` (an explicit `[strictness]` entry always wins). **Seven new advice-only lints** in `tyc-analyse/src/perf.rs`, gated by `[strictness] suggest-perf`: `perf_membership_in_loop`, `perf_list_shift_in_loop`, `perf_str_concat_in_loop`, `perf_sort_in_loop`, `perf_sorted_first`, `perf_keys_membership`, and `lazy_import_opportunity`. **Free-threading parallelisation wave** (`[python] free-threaded = true`): `auto-parallel` widens to comprehension filters, multi-argument calls, and nested pure calls; a new `auto-parallel-reductions` knob folds a provably-bounded integer accumulator loop (`mut total: int; for x in xs: total += EXPR`) into a parallel `sum(map_pure(...))`; two new advice lints (`parallel_opportunity`, `shared_mut_across_tasks`); and a `parallel-backend = "interpreters"` option tries a PEP 734 `InterpreterPoolExecutor` before falling back to threads. **Native PEP 810 lazy imports:** `[python] target = "3.15"` / `"3.15t"` lowers `lazy import ALIAS = MODULE` to CPython's native `lazy import MODULE as ALIAS` statement instead of the `typhon_runtime` helper call; 3.13/3.14 output is byte-for-byte unchanged. No new syntax; every rewrite here is opt-in or advice-only, so no previously-*correct* program changes behaviour. **v1.0.0-alpha.4** (2026-07-08) was a focused hardening pass on top of alpha.3. **Type-checker soundness:** it closes **H5**, the one HIGH finding the release-readiness review deferred — the v0.15.0 qualified↔bare tail-unification let a *locally declared* class satisfy a same-named class from another module (a user `class Response:` type-checked into an `httpx.Response`-typed slot, and vice versa). `is_assignable` now refuses that unification when the bare side names a class declared in the module being checked and the qualified side's declaration — resolved through its **exact** module key — has a different shape. The guard is evidence-gated and degrades to the previous permissive unification on any uncertainty (unresolvable module, unknown shape, facade re-export with an equivalent shape, interfaces, bare names of unknown provenance), so the example/stress corpus is byte-identically unchanged. **Diagnostics:** the secret-name keyword tables in `tyc-analyse` and the `tyc build` scan match longest-first again — `APIKEY` precedes the bare `KEY`, so `KEY_APIKEY` reports the more-specific suffix. **Supply-chain & packaging:** every third-party GitHub Action is SHA-pinned (Dependabot keeps the pins fresh), the installers resolve the newest release including pre-releases (a default install no longer silently fetches older binaries), and a round of dependency / advisory bumps lands (`crossbeam-epoch` 0.9.20 for RUSTSEC-2026-0204, plus `regex`, `memchr`, `compact_str`). No new syntax; like the alpha.2 diagnostics, the H5 fix is a conservative narrowing that only rejects programs passing a provably-different-shaped class across a module boundary, so no previously-*correct* program changes behaviour. **v1.0.0-alpha.3** (2026-07-03) was a release-readiness remediation pass (`RELEASE_READINESS_REVIEW.md`), the licensing / packaging / robustness counterpart to alpha.2's soundness sweep. **Licensing & packaging:** a repository-root MIT `LICENSE`, the upstream Ruff MIT notice vendored beside the fork, `SECURITY.md` / `CONTRIBUTING.md` / Dependabot, and release-workflow hygiene (pre-release tags flagged as such; auto-tag gated on green CI). **Compiler & VM:** identical errors at distinct source locations are all reported now (the module- and resolver-level dedupe no longer swallows the 2nd+ occurrence), nested-generic assignability is linear again (not O(2^depth)), four more flow-narrowing invalidation holes are closed, and six VM↔CPython parity gaps are fixed (cyclic-value comparison no longer aborts the process, `str.find`/`rfind`/`index`/`rindex` return character offsets, in-place list `+=`, float `%`/`//` sign & zero-division, broken-pipe `print`, `json.dumps` scalar-key coercion). **Tooling:** a 256 MiB LSP stack, atomic `tyc fmt` (temp-file + rename), a `TYC_NO_INTROSPECT` kill-switch for venv dependency introspection, Windows venv discovery, and the now-wired `[strictness] exhaustive-match` knob (previously a validated no-op). **Diagnostics/docs:** diagnostic doc URLs now point at resolvable GitHub paths (`github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/<code>.md`, not the never-deployed `typhon.dev`), and four diagnostics gained doc pages + `tyc explain` entries (`empty_collection_no_annotation`, `freeze_not_freezable`, `newtype_invalid_base`, `typing_alias_in_annotation`). No new syntax; the four flow-narrowing fixes are conservative widenings that only affect programs relying on a previously-*unsound* narrowing, so no previously-*correct* program changes behaviour. **v1.0.0-alpha.2** (2026-06-29) was the remediation of a 2026-06-28 adversarial pre-release review: a type-checker **soundness** sweep (flow narrowing of a non-local place — global / instance field — is now invalidated across an intervening call or alias write; short-circuit `and`/`or` narrowing no longer false-positives on `x is not None and x.method()`), a batch of newly-typed positions (slice reads, subscript assignments, `for` / `let` tuple-unpack targets, `match` star / or-pattern captures, walrus bindings, parameter defaults), VM↔CPython parity fixes (`bytes %`-formatting, numeric int/float/bool dict-key collapse, VM `as!` enforcement), three new **conservative** diagnostics (`tyc::not_a_context_manager`, `tyc::raise_non_exception`, `tyc::frozen_inheritance_conflict` — each fires only where the emitted Python already crashed), and config / crash robustness (`typhon.toml` rejects unknown keys / invalid `[checker] external` / `[strictness]` severities; the CLI runs on a 256 MiB worker stack). No new syntax, and no previously-*correct* program changes behaviour — but, unlike the purely-additive releases before it, the three new diagnostics do narrow the accepted surface for programs that already crashed at runtime (they now surface as build-time errors instead). **v1.0.0-alpha** (2026-06-22) was Typhon's first tagged alpha and first *feature-complete* milestone: the proven surface plus the landed type-system frontier (HKT unification, user-generic variance inference, the inter-procedural field-init audit). The prior numbered release was **v0.15.7** (2026-06-21). The language is **additive across the v0.3.0 → v1.0.0-alpha line** — every previously-accepted program continues to type-check identically (v1.0.0-alpha.2 was the first exception: its three conservative diagnostics reject a small set of programs that only ever crashed at runtime; v1.0.0-alpha.3 and v1.0.0-alpha.4 stay additive on *correct* programs — alpha.3's flow-narrowing fixes and alpha.4's H5 class-unification fix only reject code that relied on a previously-unsound narrowing). The v0.10.0 → v0.13.x line is dominated by two themes: making the in-process VM a true drop-in for `tyc build && python`, and deepening compile-time type-checking of third-party code. v0.13.0 folds in a post-release code review that fixed ten correctness issues (two VM/CPython divergences — seeded `random.sample`, and `@staticmethod`/`@classmethod` binding `self` when read through an instance — and an `incompatible_override` false positive on valid LSP-widening overrides); v0.13.1 is a six-fix patch from a playground stress round (async-`?` `missing_await`, `await`-unwrap for `asyncio.Task`/`Future`, `tyc run` resolving the whole project tree, the VM binding imported `type` sealed-union aliases, `tyc fmt` string-awareness around `#` in a `freeze let`, and `pub enum` parsing); v0.13.2 is a four-fix playground patch (`missing_return` false positive on a `match` over an impl-method call returning `Result`, `?` after a multi-line call, `gather:` inside a `case` arm not injecting `import asyncio`, and `tyc fmt` rejecting typed tuple-unpack / corrupting multi-line `freeze let`). **Two new language forms** landed since v0.9.0 — the **`enum` keyword** (v0.11.0): `enum Name:` sugars over `enum.Enum`, auto-numbering bare members with `enum.auto()`; and the **`as!` checked boundary cast** (v0.14.0): `EXPR as! TYPE` types as `TYPE` and lowers to a runtime structural check (`typhon_runtime.cast.checked_cast`), the sound one-line replacement for the `unsafe:`-block + re-assertion dance at an untyped boundary. v0.14.0 also adds the opt-in **`[emit] traceback-remap`** config (injects a `sys.excepthook` into the entry `__main__` that rewrites uncaught tracebacks to `.ty` source via the `.py.map` sidecars). The **v0.14.1 → v0.15.7** line then sharpened Typhon at the library boundary and across module boundaries: **v0.14.1** threaded `newtype` / transparent `type` alias / `enum` / `frozen` class shapes through the cross-module registry (an imported newtype now widens to its base, an imported `frozen` class write now trips `frozen_assign`, an exhaustive `match` over an imported `enum` no longer false-fires `missing_return`); **v0.14.2** added **`tyc::gather_opportunity`** (advice, on by default — flags 2+ adjacent independent `await`s that could run concurrently, including awaited method calls on imported clients) and extended `auto-gather` to fold imported `@gatherable` callees, with both advisory lints now live in the LSP; **v0.14.3** made `typhon.toml` edits refresh editor diagnostics live; **v0.15.0** made the **`as!` checked cast compose in any expression position** (structural fixpoint lowering — nested in call args, comprehensions, multi-line values, and `if`/`while`/`assert` conditions), added the **`try_result(thunk[, on_err])`** prelude combinator (exception→`Result` in one expression), and started shipping **compiler-bundled `.dty` stubs** (httpx, requests) seeded before venv introspection; **v0.15.1** is a compiler-perf + docs-a11y patch (O(N²)→O(N log N) source-map generation); **v0.15.2** fixes an `as!`/`?` preprocessor bug where a quote inside a `#` comment opened a phantom string; **v0.15.4** closes the cross-module structural-interface-conformance gap (a concrete class reaching a consumer only via an imported provider's return type or a `mod.Iface`-qualified annotation now conforms — the checker resolves class/interface shapes through the project-wide registry, not just directly-imported names; a non-conforming concrete still errors), fixes a `pub comptime let` / `pub comptime def` parse error (`pub` now stacks with `comptime`), and documents that `extend BUILTIN:` is module-local; **v0.15.5** makes `extend BUILTIN:` cross module boundaries end-to-end (type checker, build/codegen, and VM runtime — `title.slug()` in a consumer that imported a module declaring `extend str: def slug(...)` now resolves); and **v0.15.6** is a stress-test robustness sweep (~198-program corpus) that clears a cluster of type-checker false positives — custom exception classes (`class FooError(Exception):` is no longer auto-`@dataclass`'d, so `raise FooError("msg")` works), nested/list-star/tuple-of-union `match` exhaustiveness, `isinstance`-narrowed bare containers usable parametrically, iterator-protocol classes conforming to `Iterator[T]`, and `dict`-view set operations — plus VM parity gaps (`**kwargs` order, slice deletion, `__format__` dispatch, `bytes` operators, exception `str`/`repr`) and two small additive features: flow-sensitive attribute narrowing (`if self.x is None: return …`) and a VM `abc` module shim; and **v0.15.7** deepens third-party introspection — **method-call arity-checking** (a missing required arg to `model.fit()` / `df.merge()` now errors at check time) plus introspection-robustness and type-checker false-positive fixes (proxy-member survival, implicit-Optional `x: T = None`, multi-segment `pkg.sub.Thing()` calls, `Counter +/- Counter`, `plain class`/`class!` custom-`__init__` kwargs, `__call__`→`Callable`, and a VM `str.isupper()`/`islower()` parity fix) with compiler-perf passes. Highlights:

- **VM completeness (v0.10.0 / v0.11.0):** the tree-walking VM now dispatches operator / reflected / rich-comparison dunders on user instances, honours `__str__` / `__repr__` / `__len__` / `__getitem__` / `__contains__` / `__call__` / `__post_init__`, runs `@property` / `@classmethod`, materialises finite generators eagerly (`yield` / `yield from`, capped at 1M items), models `type(x)` as a real type object, and ships `Value::Complex`, dict-view kinds (`dict.keys()/.values()/.items()`), native `enum` / `datetime` / `pathlib` / `collections.defaultdict` shims, and a long tail of string / set / math / json / time builtins. VM value semantics now match CPython (value-based dataclass equality keyed on class identity, `Name(field=value)` repr, hashable instances, order-independent set equality, shortest-round-trip float repr).
- **Deep compile-time library introspection (v0.12.0):** venv signature introspection now captures parameter and **return annotations**, so a wrong-*typed* argument to a fully-typed third-party dependency — **function or constructor** — is caught by `tyc check` / `tyc build` via the same `tyc::type_mismatch` machinery your own code uses (this also closed an in-project constructor-argument soundness hole). A dependency that can't be introspected now **warns** instead of silently skipping its checks (`[strictness] unintrospectable-dependency`, default `warn`). The introspection logic moved into a new shared crate **`tyc-venv`**, consumed by both the CLI and `tyc lsp` (so the editor surfaces third-party arg/type squiggles live).
- **`ty` typeshed integration (v0.12.0, Phase 1):** `[checker] external = "ty"` (or `--with-ty` on `tyc build` / `tyc check`) runs Astral's `ty` over the emitted Python — the only path that covers C-extension and stdlib APIs runtime introspection can't see (`os.path.join(1, 2)` now caught) — with `ty`'s diagnostics re-attributed to your `.ty` source. The embedded in-process `ty` (Phase 2) was prototyped and proven feasible but is **not shipped** (it needs a git dependency `cargo deny` disallows; it adds no capability the subprocess path lacks).

v0.8.0 introduced one runtime behaviour change carried forward: the VM uses arbitrary-precision integers (`num_bigint::BigInt`), so programs that relied on silent i64 wrap-around compute mathematically-correct results (and from v0.11.0 the VM's value semantics above also produce different — correct — results where they previously diverged). Several long-standing frontier items landed in **v1.0.0-alpha**: **HKT unification** (constructor variables like `F` in `class Functor[F[_]]` now bind against concrete heads, with a `tyc::kind_mismatch` diagnostic on wrong arity / conflicting binding), **user-generic variance inference** (covariant / contravariant type-params inferred from usage, including across module boundaries, with `@covariant` / `@contravariant` overrides), variance through generic interface bounds, **2-member non-nullable union modelling** at the introspection boundary, and the **general inter-procedural field-init audit** (partial instances tracked across helper chains). The remaining open frontier work (embedded `ty` Phase 2, accumulator-loop parallelisation, typeshed-backed pure-extension checking) is tracked in `TYPE_SYSTEM_FRONTIER.md`.

LLMs have no prior knowledge of Typhon. This skill is the field reference. **Trust the docs and the compiler over assumptions from Python or any superset.** When this skill and a doc disagree, the doc wins. When the docs and the compiler disagree, the compiler wins — verify with `tyc check`.

The canonical sources are:

- **`README.md`** — pitch, release table, workspace layout, top-level project status.
- **`CHANGELOG.md`** — every release back to v0.1.0. v0.3.0–v0.12.0 are the live window.
- **`docs/long-term-plan.md`** — source of truth for design decisions.
- **`docs/language.md`** — the type system, error handling, async, `let`/`mut`, comptime.
- **`docs/cheatsheet.md`** — 30-second syntax refresher (also `tyc cheatsheet`).
- **`docs/cli.md`** — every `tyc` subcommand.
- **`docs/configuration.md`** — every key in `typhon.toml`.
- **`docs/vm.md`** — the in-process tree-walking VM (default execution mode for `tyc run`).
- **`docs/architecture.md`** — pipeline + crate layout.
- **`docs/diagnostics/*.md`** — one page per `tyc::` code; also surfaced via `tyc explain <code>`.
- **`docs/guides/01..10-*.md`** — the teaching surface; read in order the first time.
- **`docs/install.md`** — pre-built binaries (Linux x86_64/aarch64, macOS Apple Silicon/Intel, Windows x86_64) plus source build.
- **`docs/ty-integration.md`** — how `tyc ty` and the `[checker] external = "ty"` build hook cooperate with Astral's checker (Phase 1 shipped; Phase 2 embedded prototyped, not shipped).
- **`docs/performance-baseline.md`** — measured numbers we don't want to regress.
- **`docs/roadmap.md`** / **`docs/risks.md`** / **`docs/prior-art.md`** / **`docs/findings.md`** — context.
- **`examples/NN-*/`** — 32 curated stdlib-only exercises (sparsely numbered `01`–`68`, leaving room between topics).
- **`examples/apps/01..15-*/`** — 15 production-shaped multi-file projects (event-sourced banking, distributed KV, mini-compiler, search engine, GraphQL server, game ECS, trading engine, ML orchestrator, web crawler, task scheduler, real-time game server, static site generator, vector DB, API gateway, stream processor).
- The full example corpus is **259 `.ty` files** (exercises + apps + `examples/testing/`); the v0.12.0 third-party-introspection work was verified against it with zero false positives.
- **`tyc/vendor/README.md`** — Ruff fork rationale.
- **`editors/vscode/README.md`** — reference VS Code extension (v0.2.3).
- **`docs-site/`** — the published Astro + Starlight documentation site (`src/content/docs/*.mdx`); self-contained, not generated from `docs/`. Deployed to GitHub Pages.

This skill ships sibling files for the long-tail detail:

- **[REFERENCE.md](REFERENCE.md)** — every Typhon syntactic form, side-by-side with its emitted Python.
- **[CLI.md](CLI.md)** — verbose subcommand reference with every flag, exit code, and example.
- **[PITFALLS.md](PITFALLS.md)** — extended catalogue of the surprises every newcomer hits.
- **[DIAGNOSTICS.md](DIAGNOSTICS.md)** — exhaustive `tyc::` code reference, grouped by category.
- **[COOKBOOK.md](COOKBOOK.md)** — canonical patterns extracted from `examples/`.
- **[RUNTIME.md](RUNTIME.md)** — the generated `typhon_runtime` package and the in-process VM.
- **[PACKAGING.md](PACKAGING.md)** — multi-file projects, `__init__.ty`, `pub *` aggregation.
- **[references/](references/)** — 20 curated, compile-clean single-file `.ty` programs (one feature area each) plus an index `README.md`. Lifted verbatim from the project `examples/` corpus, so each one type-checks and runs on the matching release. Start here for a worked example of any feature.

**Installing this skill into another project:** run `tyc install skill` from any project where `tyc` is on `PATH` — it writes the whole `.claude/skills/typhon/` tree (this `SKILL.md`, every sibling reference, and the `references/` examples) into the current directory. `tyc install skill --force` overwrites an existing copy; `tyc install skill --dir PATH` targets a different project root. See [CLI.md](CLI.md#tyc-install) and [§14](#14-tyc-subcommand-reference).

---

## 1. When to invoke this skill

Trigger automatically when the session involves any of:

1. **Authoring or editing `.ty` source.** Re-read the relevant guide section before writing significant code — Typhon's surface looks Python-like but at least eight rules diverge silently (Section 4).
2. **Editing `.dty` stubs.** These are Typhon's source of truth for third-party Python APIs; they emit `.pyi` for interop. Drift is caught by `tyc check --stubs` and `tyc stubtest`.
3. **Editing `typhon.toml`.** Every strictness flag and emit knob has subtle defaults — see [§13 configuration reference](#13-typhontoml-reference).
4. **Working inside the Rust compiler** (`tyc/crates/`). The pipeline is `syntax → resolve → types → analyse → desugar → emit → format`, backed by a Salsa DB. See [§16 compiler architecture](#16-compiler-architecture).
5. **Migrating `.py` → `.ty`.** Use `tyc migrate` first — it rewrites `Optional[T]` → `T?`, `Generic[T]` → PEP 695, drops `@dataclass`, rewrites `NewType` and `Protocol`. Then resolve diagnostics manually.
6. **Debugging a `tyc::...` diagnostic.** The [DIAGNOSTICS.md](DIAGNOSTICS.md) catalog is the fastest lookup; `tyc explain <code>` works offline.
7. **Onboarding someone to Typhon.** Walk them through the [§3 cheat sheet](#3-cheat-sheet) first, then the guides.

---

## 2. Hello, Typhon — the shortest realistic flow

```bash
# 1a. Install a pre-built binary (macOS / Linux):
curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh

# 1b. Windows (PowerShell):
iwr -useb https://raw.githubusercontent.com/codehalwell/typhon/main/install.ps1 | iex

# 1c. Or build from source (from repo root):
cd tyc && cargo build --release && cd ..
alias tyc="$PWD/tyc/target/release/tyc"

# 2. Scaffold a new project
tyc init hello && cd hello
# → typhon.toml, src/main.ty, tests/

# 3. Edit src/main.ty (see below)
# 4. Iterate
tyc fmt src/         # whitespace-normalise + ruff format wrap
tyc check src/       # parse + resolve + type-check, no output artifacts
tyc run              # default: execute via the in-process tree-walking VM
tyc build            # full pipeline → build/main.py + build/.sourcemaps/*.py.map
python build/main.py
```

A canonical `src/main.ty`:

```python
import sys

def main() -> None:
    let name: str = sys.argv[1] if len(sys.argv) > 1 else "world"
    print(f"Hello, {name}")

if __name__ == "__main__":
    main()
```

The emitted `build/main.py` is byte-similar (formatting aside). **Production never installs anything Typhon-specific** — only a small generated `typhon_runtime/` package when you actually use `Result`, `go`, `lazy let`, `freeze let`, or auto-parallel comprehensions.

---

## 3. Cheat sheet

The 30-second mental model. Every later section in this skill is detail under one of these bullets.

| Topic | Typhon | Emitted Python |
|---|---|---|
| Local binding | `let x: int = 1` / `mut x: int = 1` | `x: int = 1` |
| Module binding | `X: int = 1` (implicit `let`) or `mut X: int = 1` | `X: int = 1` |
| Declare-then-assign (v0.7.0) | `let loaded: Cfg` then assign on every non-diverging arm | `loaded: Cfg` then `loaded = …` |
| Typed tuple unpack (v0.3.1) | `let (a: int, b: str) = pair()` | hidden temp + per-element typed assigns |
| Deep-immutable binding | `freeze let CFG = {"port": 8080, "hosts": ["a", "b"]}` | `CFG = __typhon_freeze__({...})` (deep-freezes value) |
| Public name | `pub let API_VERSION: str = "v1"` / `pub class Foo: ...` | synthesised `__all__ = [...]` at top of module |
| Package re-export (v0.7.0) | `pub *` in `__init__.ty` | aggregates sibling modules + sub-packages |
| Nominal alias | `newtype UserId = int` | `UserId = NewType("UserId", int)` — asymmetric |
| Same-newtype arithmetic (v0.7.0) | `let x: Index = a + b` | preserved across `+ - * // % **`; `/` still widens to `float` |
| Nullable | `name: str?` | `name: str \| None` |
| Optional default | `name: str? = None` (no auto-default) | `name: str \| None = None` |
| Class | `class User: id: int` | `@dataclass(slots=True) class User: id: int` |
| Pydantic model | `model ApiUser: id: int` | `class ApiUser(BaseModel): model_config = ConfigDict(extra="forbid"); id: int` |
| Frozen class | `class P frozen: x: float` | `@dataclass(slots=True, frozen=True)` |
| Plain class (no decorator) | `plain class Bag: items: list[str]` | bare `class Bag:` (no `@dataclass`, no synthesised `__init__`) |
| Raw class (framework base) | `class! M(nn.Module): layer: nn.Linear` | bare `class M(nn.Module):` with synthesised `__init__` calling `super().__init__()` |
| Enum (v0.11.0) | `enum Color: RED; GREEN` / `enum C: RED = 1` | `class Color(enum.Enum): RED = enum.auto(); ...` (`import enum` injected) |
| Methods | `impl User: def display(self) -> str: ...` | merged into the class body |
| Extend foreign class | `extend User: def x(self) -> int: ...` | merged at desugar |
| Extend built-in | `extend str: def slug(self) -> str: ...` | extracted to `__typhon_ext_str__slug__` free fn + call-site rewrites |
| Sealed union | `type Shape = Circle \| Rectangle` | `Shape = Circle \| Rectangle` (just a type alias) |
| `impl` on sealed union (v0.6.0) | `impl Shape: def area(self) -> float: ...` | distributes the method to every variant |
| Exhaustive match | `match s: case Circle(r): ...` (no `_` needed) | vanilla Python `match` |
| Result type | `Result[T, E]`, `Ok(v)`, `Err(e)` | generated `typhon_runtime.Ok/Err` dataclasses |
| Result combinators (v0.6.0) | `r.map(f) / r.map_err(g) / r.and_then(h) / r.or_else(k)` | method calls on the runtime classes |
| Exception→Result (v0.15.0) | `try_result(lambda: f(), lambda e: str(e))` | `from typhon_runtime import try_result` + call; types as `Result[T, E]` |
| Boundary `rescue` (v1.0.0-alpha) | `let n = int(s) rescue e: f"bad: {e}"` / `rescue e: ERR:`-block | postfix → `try_result(lambda: …, lambda e: …)?`; block → `try`/`except … return Err(…)` |
| Error propagation | `let n: int = f()?` | inline `isinstance(_t, Err): return _t; n = _t.value` |
| Result chain | `with a = f()?, b = g()?: ...  else err: ...` | sequenced if-isinstance ladder |
| Generic fn | `def first[T](xs: list[T]) -> T?:` | same (PEP 695) |
| Generic class | `class Box[T]: value: T` / `impl[T] Box[T]:` | preserved (PEP 695) |
| HKT (v0.5.0 scaffold; unification landed v1.0.0-alpha) | `class Functor[F[_]]:` | preserved (PEP 695); `F[A]` unifies against a concrete head |
| Interface | `interface Drawable: def draw(self) -> None` | `class Drawable(Protocol): ...` |
| Pipe | `a \|> f() \|> g(arg)` | `g(f(a), arg)` |
| Guard | `guard x = expr else: return ...` | `if expr is None: return ...; x = expr` |
| Parallel awaits | `gather: a = f(); b = g()` | `async with asyncio.TaskGroup() as _tg: ...` |
| Best-effort gather | `gather(strategy="best-effort"):` | `asyncio.gather(..., return_exceptions=True)` |
| Spawn | `go f(x)` / `go f(x) -> task` | `typhon_runtime.tasks.spawn(...)` (strong-ref registry) |
| `await` middleware-callable (v0.7.0) | `let r: Resp = await next(req)` where `next: Callable[[Req], Awaitable[Resp]]` | unwraps to `Resp` |
| Lazy module | `lazy import np = numpy` | bespoke `__TyphonLazy_np_` proxy with double-checked locking |
| Lazy module-level let | `lazy let CFG: Config = load()` | sentinel-cached `lazy_let(lambda: load())` |
| Lazy class-level let | `lazy let cfg: Config = ...` inside class | `@cached_property` |
| Comptime constant | `comptime let PORT: int = int(env("PORT", "8080"))` | inlined literal at build time |
| Comptime type-value (v0.5.0) | `comptime let T: type = int` | inlined; supports 8 primitive heads |
| Pure assertion | `@pure def f(...) -> T:` | nothing emitted unless `@memo` too |
| Memoised | `@memo def fib(n: int) -> int:` | `@functools.cache` |
| Auto-gather opt-in | `@gatherable async def fetch_x(...) -> X:` | enables auto-gather rewrite |
| Unsafe boundary | `unsafe: let x = mystery_lib()` | `if True:` (scope-preserving) |
| Checked boundary cast (v0.14.0) | `let d = resp.json() as! dict[str, int]` | `d = __typhon_checked_cast__(resp.json(), dict[str, int])` (runtime-checked) |
| `with` as-target typing (v0.7.0) | `with conn() as c:` types `c` from `__enter__` / `@contextmanager` factory's yield | works for async too |

Run `tyc cheatsheet` for the same table at the terminal.

---

## 4. The eight rules every Typhon program follows

These are the rules behind every "but the same code works in Python" surprise. The first five are the foundational ones; the last three landed in v0.3.0 → v0.7.0.

### Rule 1 — Every parameter and return type is annotated

```python
def add(a: int, b: int) -> int:    # ✅
    return a + b

def add(a, b):                     # ❌ tyc::missing_annotation
    return a + b
```

`-> None` is mandatory for sync functions that return nothing. There is no inference fallback. This is `[strictness] no-implicit-any = true` (default on, the implicit-any escape is parsed for forward-compat but currently has no effect — the check is always on).

### Rule 2 — Local bindings declare `let` or `mut`

```python
def demo() -> None:
    let pi: float = 3.14159      # immutable
    mut counter: int = 0         # mutable
    counter = counter + 1        # ✅
    # pi = 3.14                  # ❌ tyc::immutable_assign
```

Module-level bindings default to `let` if you skip the keyword — but a *local* `name = "x"` with no keyword is `tyc::missing_binding_kind`. Reach for `mut` only when you actually rebind.

Carve-outs (no keyword required):
- `global NAME` / `nonlocal NAME` declarations bind the outer-scope variable; the bareword assignment that follows refers to that binding.
- `gather:` block bindings (`gather: a = fetch_a(); b = fetch_b()`) — the keyword itself introduces immutable single-assignment names.
- Walrus operator: `if (n := len(xs)) > 3:` introduces `n` as an implicit `let` binding; rebinding `n` later requires `mut`.
- `for` / `with` / `case` / `except` targets are bindings, not assignments — they don't take `let`/`mut` and don't collide with outer `let` bindings of the same name (v0.6.0+).

### Rule 3 — `T` cannot hold `None`

```python
def greet(name: str) -> None: ...
def find(id: int) -> str?: ...   # str? == str | None

let found: str? = find(1)
greet(found)                     # ❌ tyc::nullable_use

if found is not None:
    greet(found)                 # ✅ narrowed to str

guard f = found else: return     # ✅ same effect, prettier
greet(f)
```

Narrowing forms the checker understands: `is None`, `is not None`, `isinstance(x, T)`, `guard`, early-return `if x is None: return`, exhaustive `match`, **ternary** `body if test else orelse` (v0.7.0), the truthy-falsy union picks of `or` / `and` (Python semantics). De Morgan narrowing (`if not (A or B): return` refines both operands afterwards) lands in v0.4.0.

`T?` emits as `T | None`.

### Rule 4 — Methods live in `impl`, not in `class`

```python
class User:
    id: int
    name: str

impl User:
    def display(self) -> str:    # explicit `self`; use `self.NAME`
        return f"{self.name} (#{self.id})"
```

Writing `__init__` inside `class` is rejected (`tyc::manual_init`) — the constructor is generated. Methods in the class body fire `tyc::method_in_class_body` (severity `warn` by default; configurable via `[strictness] methods-in-class-body`). The bare-identifier sugar where `name` resolves to `self.name` is **not** implemented; write `self.NAME` explicitly.

`impl` blocks can span multiple files via `impl ClassName:` (same project) and `extend ClassName:` (cross-module, identical merge semantics). `impl` on a **sealed-union alias** distributes the methods to every variant automatically (v0.6.0):

```python
type Event = TaskStarted | TaskFinished | TaskFailed

impl Event:
    def is_terminal(self) -> bool:
        match self:
            case TaskStarted(_): return False
            case TaskFinished(_) | TaskFailed(_): return True
```

### Rule 5 — `Any` only enters through `unsafe:` or `.dty` stubs

```python
import messy

let data = messy.fetch()         # ❌ tyc::missing_annotation / implicit Any

unsafe:
    let data = messy.fetch()     # ✅ inside the region
let parsed: dict[str, int] = ... # re-assert at the boundary
```

`unsafe:` is a *lexical region*, not a per-value annotation. Values inside acquire a hidden `Unsafe[T]` marker that cannot cross out into a concrete-typed context without a re-assertion (annotation, narrowing, or cast). The block lowers to `if True:` so scope rules are unchanged. Smuggling an `Unsafe[T]` outward fires `tyc::unsafe_value_leak`.

For long-lived dependencies, write a `.dty` stub instead.

### Rule 6 — Exhaustive `match` on sealed unions

```python
type Shape = Circle | Rectangle | Triangle

def area(s: Shape) -> float:
    match s:
        case Circle(radius):       return 3.14159 * radius * radius
        case Rectangle(w, h):      return w * h
        # ❌ tyc::non_exhaustive_match — Triangle uncovered
```

Severity controlled by `[strictness] exhaustive-match` (default `"error"`). Use `case _:` only when a catch-all is genuinely the intent (it disables exhaustiveness for the remaining variants). Keyword patterns (`case TaskStarted(task_id=tid):`) also satisfy exhaustiveness (v0.6.0).

### Rule 7 — Errors flow as `Result[T, E]`, not exceptions

```python
def parse_port(raw: str) -> Result[int, str]:
    if not raw.isdigit():
        return Err(f"not a number: {raw}")
    let n: int = int(raw)
    if n < 1 or n > 65535:
        return Err(f"out of range: {n}")
    return Ok(n)
```

`?` propagates `Err` cleanly inside any `Result`-returning function. Bridging to exceptions happens at library boundaries via a small `try` shim. See [§9 error handling](#9-error-handling).

### Rule 8 — Definite assignment for declare-only `let` (v0.7.0)

```python
def parse(raw: str) -> Result[Cfg, str]:
    let loaded: Cfg                          # declare-only; OK in v0.7.0+
    match _load(raw):
        case Ok(v):  loaded = v
        case Err(e): return Err(e)
    return Ok(loaded)                        # ✅ both arms either assign or diverge
```

The resolver tracks each uninitialised `let` declaration's span; the first subsequent assignment **is** the initialiser. The second assignment to a `let` still fires `tyc::immutable_assign`. `mut NAME: T` without initialiser is also accepted (any number of subsequent assignments legal). Reads on a path where the binding hasn't been assigned fire `tyc::use_of_uninitialised`; `if`/`elif`/`else` and `match` arms each count as separate first-assignment paths; `return` / `raise` / `continue` / `break` arms are excluded from the intersection.

---

## 5. Type system

### 5.1 Primitives and widening

| Type | Notes |
|---|---|
| `int` | Arbitrary precision (matches CPython). The VM's `VmInt` keeps `i64`-sized values inline and promotes to a `BigInt` on overflow (v0.8.0 / v1.0.0-alpha.5). |
| `float` | 64-bit IEEE 754. |
| `bool` | **Subtype of `int`** (v0.4.0). `let x: int = True` type-checks; `let b: bool = 1` does not — assignability is one-way. |
| `str`, `bytes` | Identical to Python. |
| `None` | Inhabitant of unit; only assignable where `T?` allows. |

`int` widens to `float` (`let y: float = 3` ✅). `float` does **not** narrow to `int` — write `int(x)` / `round(x)`. `bool` flows into `int` for arithmetic: `1 + True` types as `int`; `-True` types as `int`.

### 5.2 Collections

Element types are required:

```python
let xs: list = [1, 2, 3]         # ❌ implicit Any element / missing_annotation
let xs: list[int] = [1, 2, 3]    # ✅
let cs: dict[str, int] = {"a": 1}
let pts: tuple[float, float] = (1.0, 2.0)
let nums: tuple[float, ...] = (1.0, 2.0, 3.0)
```

`dict.get(k)` returns `V?`, not `V`. Either narrow or use `d[k]` (typed `V`, may raise `KeyError`).

**Fixed-arity tuple covariance** (v0.4.0): `tuple[int, int]` widens both slots to `float` at the assignment site. Mutable containers (`list`, `dict`, `set`) remain invariant.

TypedDict-style dict literals match field shapes (v0.3.0): `let alice: User = {"id": 1, "name": "Alice"}` matches a `model User` declaration field-by-field.

Set difference: `set - set` and `frozenset - frozenset` type-check (v0.5.2).

### 5.3 `T?` and flow narrowing

```python
def find_user(id: int) -> str?: ...

let raw: str? = find_user(1)
if raw is None:
    return
greet(raw)                       # ✅ raw narrowed to str

guard r = find_user(1) else: return   # equivalent, prettier
greet(r)
```

`isinstance(x, T)` also narrows. Internally `T?` is `Nullable[T]`; emission is `T | None`. `while`-condition narrowing applies test-implied narrowings to the body (v0.3.0). Ternary narrowing (v0.7.0): `let r: int = x if x is not None else 0` types `x` as the non-null form inside the `x` arm.

### 5.4 Generics — PEP 695 only

```python
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]

class Box[T]:
    value: T

impl[T] Box[T]:
    def map[U](self, f: Callable[[T], U]) -> Box[U]:
        return Box(value=f(self.value))

type Vec[T] = list[T]            # transparent alias
type Pair[A, B] = tuple[A, B]
```

Inference is bidirectional and recursive; `pair(1, "two")` for `pair[T](a: T, b: T)` widens `T` to `int | str`. **Never import `TypeVar` from `typing`** — that path is rejected with `tyc::typevar_import_rejected`. Same for the deprecated capitalised aliases (`List`, `Dict`, `Tuple`, `Set`, `FrozenSet`, `Type`) → `tyc::typing_alias_deprecated`.

**HKT (scaffold v0.5.0; unification landed v1.0.0-alpha):** the parser accepts `F[_]` as a type-constructor parameter (`class Functor[F[_]]:`), and unification now binds a constructor variable against a concrete head — `F[A]` against `list[int]` binds `F = list, A = int` and substitutes in the return type, in-module and across module boundaries. Wrong arity (`F[A, B]` vs `list[int]`) and conflicting constructor binding emit `tyc::kind_mismatch`. Function-level HKT params (`def f[F[_]]`), non-class constructor application, and constructor composition remain deferred — see `TYPE_SYSTEM_FRONTIER.md`.

**Cross-module generic method dispatch propagates class TypeVars (v0.7.0):** `s: Stream[int]; s.map(f)` records `Callable[[int], U]` as the expected parameter and returns `Stream[U]`. Same for field access on a generic — `r: RecordEnv[int]; r.payload` is `int`, not bare `T`.

Generics erase at emit time (PEP 695 syntax is preserved on the 3.13+ default).

### 5.5 Interfaces (structural)

```python
interface Drawable:
    def draw(self) -> None
    def width(self) -> float

class Button:
    label: str

impl Button:
    def draw(self) -> None: print(self.label)
    def width(self) -> float: return float(len(self.label) + 4)

def render(d: Drawable) -> None:
    d.draw()

render(Button(label="x"))        # ✅ structurally matches
```

Emits as `class Drawable(Protocol): ...`. **`isinstance(x, Drawable)` is rejected** (`tyc::interface_isinstance`) because Python's `@runtime_checkable` only checks attribute *presence*, not signatures. Refactor to a sealed union or write an explicit predicate. Interface methods match against methods in the candidate, not against fields of callable type — mixing is rejected (`tyc::interface_not_conforming`).

### 5.6 Sealed unions

```python
type Shape = Circle | Rectangle | Triangle

class Circle:    radius: float
class Rectangle: width: float; height: float
class Triangle:  base: float;  height: float
```

(There is **no `sealed union NAME:` keyword block form** — a sealed union is written as a `type` alias over separately-declared variant classes, as above. Earlier docs/cheatsheets showed a `sealed union Shape:` block; that does **not** parse and has been corrected.) Variants can be parametric (`type EventEnvelope[T] = RecordEnv[T] | WatermarkEnv | BarrierEnv` — some refer to `T`, others don't). For nullary variants, write `class Nil frozen: pass` and match with `case Nil():` (two empty parens, not `case Nil(_):`).

The match is statically verified exhaustive. Add `Square` to the alias → every match becomes `tyc::non_exhaustive_match` until handled. Cross-module variant flow works (v0.6.0) — variant `A(...)` flows into an `Event`-typed slot in a consumer module even when the alias is declared in another package.

### 5.7 `class` vs `model` vs `plain class` vs `class!` vs `enum`

| Form | Emits | Use when |
|---|---|---|
| `class Foo:` | `@dataclass(slots=True)` | Plain value type (default). |
| `class Foo frozen:` | `@dataclass(slots=True, frozen=True)` | Immutable value type. Field reassignment → `tyc::frozen_assign`. |
| `model Foo:` | `class Foo(BaseModel):` + `model_config = ConfigDict(extra="forbid")` | Validated input at a system boundary. Pydantic dep required. |
| `interface Foo:` | `class Foo(Protocol):` (bodies `...`) | Structural contract. |
| `plain class Foo:` | bare `class Foo:` (no decorator, no `__init__`) | Metaclass-driven libs, descriptor-based models (Textual, Django ORM, SQLAlchemy declarative). |
| `class! Foo(Base):` | bare `class Foo(Base):` + synthesised `__init__` calling `super().__init__()` and assigning fields | Subclassing a framework base that owns its own `__init__` (torch.nn.Module, custom exceptions). |
| `enum Foo:` (v0.11.0) | `class Foo(enum.Enum):` with `enum.auto()` for bare members | A fixed set of named members. Sugar over `enum.Enum`. |

#### Mutable-default rewrite

Bare mutable literals (`tags: list[str] = []`) are rewritten at desugar to `dataclasses.field(default_factory=list)`. Applies to `class` and `class … frozen`; **skipped** for `model`, `interface`, and `class!`.

#### Auto-skip for framework bases

A `class Foo(Base):` whose `Base`'s last identifier segment is `Enum`, `IntEnum`, `StrEnum`, `Flag`, `IntFlag`, `ABC`, `ABCMeta`, `Protocol`, `TypedDict`, `NamedTuple`, `BaseModel`, or `App` emits without `@dataclass`. Project bases can be added via `[emit] skip-decoration-bases = ["MyBase", ...]`. Auto-skip drops only the decorator; it does not synthesise `__init__`. Use `class!` when you need both.

#### Field default ordering (v0.7.0)

`@dataclass` rejects a non-default field after a defaulted one (would raise `TypeError` at import). `tyc::field_default_ordering` catches it at check time. Move every non-defaulted field above defaulted ones, or use a factory.

#### `class!` constructor synthesis rules

- No `@dataclass` decorator.
- `__init__` auto-synthesised when the body has no user `def __init__` and ≥1 base is present:
  - Calls `super().__init__()` first (no positional/keyword args — pass them via the body if needed).
  - Assigns each annotated field through `self`, in source order. Non-defaulted fields are positional; defaulted fields follow.
- **Class-level field defaults are stripped from the body** when `__init__` is synthesised (prevents PyTorch-style double evaluation as a class attribute then per-instance).
- Annotations remain visible to type checkers.
- A hand-written `__init__` is preserved verbatim.

#### Inheritance

Single inheritance is the supported form (`class Dog(Animal):`). Subclass constructors accept inherited fields (v0.4.0): `Dog(name="Rex", breed="Husky")` works against `class Dog(Animal):` where `name` lives on `Animal`. Dataclass inheritance ordering rules still apply across the MRO — non-defaults before defaults all the way up.

#### `enum` (v0.11.0)

`enum Name:` declares an `enum.Enum` subclass — the same kind of sugar `model` is over `pydantic.BaseModel`. Bare members auto-number via `enum.auto()`; an explicit `MEMBER = value` is preserved, and a subsequent bare member resumes `enum.auto()` numbering from the last value — standard CPython `enum` semantics (e.g. `A = 10` then a bare `B` yields `B = 11`, not `2`). `tyc-syntax` preprocesses the header / body, `tyc-emit` injects `import enum`, `tyc-resolve` adds `enum` to the builtin prelude so the form type-checks before the import is rewritten in, and `tyc fmt` round-trips it. The VM (v0.11.0) resolves `enum.Enum` / `enum.auto()` natively, materialises members in declaration order, and matches CPython's `<Shape.CIRCLE: 1>` member repr.

```python
enum Shape:                       # → class Shape(enum.Enum):
    CIRCLE                        #       CIRCLE = enum.auto()
    SQUARE                        #       SQUARE = enum.auto()
    TRIANGLE                      #       TRIANGLE = enum.auto()

enum Color:                       # → class Color(enum.Enum):
    RED = 1                       #       RED = 1
    GREEN = 2                     #       GREEN = 2
    BLUE = 4                      #       BLUE = 4
```

(You can still subclass `enum.Enum` directly via the auto-skip path — `class Suit(enum.Enum):` emits without `@dataclass` — but `enum Suit:` is the idiomatic form.)

### 5.8 `impl` and `extend`

- `impl ClassName:` attaches methods to a class declared in the same project. Multiple `impl` blocks for the same class merge at desugar; they can live in different files.
- `impl[T] ClassName[T]:` introduces type parameters scoped over the methods; methods may add their own (`def map[U](...)`).
- `impl OtherType:` on an **alias** of a sealed union distributes the methods to every variant (v0.6.0). Duplicate-method check fires when the same method exists on both `impl Union:` and `impl Variant:`.
- `extend ClassName:` is `impl`'s twin for cross-module method addition. Same merge semantics.
- `extend BUILTIN:` (`str`, `list`, `int`, `dict`, …) extracts each method to a module-level free function `__typhon_ext_<TYPE>__<METHOD>__`, and rewrites `x.method(...)` to the free-function call **whenever the receiver `x` is statically annotated as that built-in**. No monkey-patching — un-annotated receivers still raise `AttributeError` at runtime. `extend list[int]:` (parametric target) fires `tyc::extend_builtin` — drop the brackets. **As of v0.15.5, `extend BUILTIN:` crosses module boundaries** — importing a module that declares `extend str: def slug(...)` makes `title.slug()` resolve in the consumer (the type checker, build/codegen, and VM all propagate the extension registry). In earlier releases this was module-local; the free-function workaround (`pub def to_slug(s: str) -> str: …`) still works but is no longer necessary.

### 5.9 `unsafe:` boundary

```python
def parse() -> int:
    unsafe:
        let v = mystery_lib.get_int()    # would be tyc::missing_annotation outside
        let checked: int = int(v)        # re-assert before crossing out
    return checked
```

Inside `unsafe:`, expressions that would otherwise infer `Any` bind freely. Values acquire a hidden `Unsafe[T]` marker that cannot flow into a concrete `T` context outside the block. Block lowers to `if True:` for scope preservation; the checker tracks an `unsafe_depth` counter. Smuggling `Unsafe[T]` outward fires `tyc::unsafe_value_leak`.

For long-lived dependencies, write a `.dty` stub instead. Common idiom: an `unsafe:` block always ends in `return checked` or `raise RuntimeError("unreachable")` — never let the block be the last thing in a non-`-> None` function.

### 5.10 `as!` — checked boundary cast (v0.14.0)

`EXPR as! TYPE` is the one-line, **sound** alternative to an `unsafe:` block plus re-assertion when you cross a single untyped boundary (a `sqlite3` row, `resp.json()`, a `redis.get` payload):

```python
let data = resp.json() as! dict[str, int]    # no unsafe: block needed
let uid  = row[0] as! int
```

- The checker types the whole expression as `TYPE`, so the boundary value (which may be `Any`) flows in freely — no `unsafe:` region, no `unsafe_value_leak` footgun. It rides the same machinery as `type[T]` generic inference, not a bespoke special case.
- It lowers (in `tyc-syntax`) to `__typhon_checked_cast__(EXPR, TYPE)`, resolved via an injected `from typhon_runtime.cast import checked_cast as __typhon_checked_cast__`. At runtime `checked_cast` verifies the value's shape against `TYPE` **recursively** (scalars, `list[X]` / `set[X]` / `frozenset[X]`, `dict[K, V]`, fixed- and variadic-`tuple[...]`, unions / `Optional`) and raises `TypeError` on a mismatch. So unlike a static-only re-assertion (which trusts the boundary blindly) or TypeScript's unchecked `as`, an `as!` can only let through values it can't *prove* wrong. `Any` / `object` targets and shapes it can't model fall back to acceptance.
- `int → float` (and `bool → int`) widening is honoured, so a JSON int cast `as! float` does not spuriously fail.
- The in-process VM runs the **same recursive structural check** as the compiled path: it interprets the `as!` type descriptor and raises `TypeError` on a wrong-shaped value under `tyc run` too, so the cast is a faithful drop-in (a wrong cast no longer slips through the VM). `tyc fmt` preserves the surface syntax. (Only the rare *indirect* reference to `checked_cast` — passed as a value rather than called directly with two args — degrades to identity, since the type descriptor isn't available as an AST there.)

**Scope (v0.15.0):** the lowering is **structural** (a fixpoint rewrite with bracket-, string-, and comment-awareness), so `as!` composes wherever an expression can appear — value positions (`=` / `op=` / `return` / `yield` / bare expression), **nested inside call arguments** (`foo(row[0] as! int, y)`), inside comprehensions / collection literals (`[x as! int for x in xs]`), **statement conditions** (`if raw as! bool:`, `while …`, `assert …`), and across a **value expression that spans multiple physical lines** (the left operand may run over several lines as long as it stays bracket-balanced). A quote inside a `#` comment no longer derails the scanner (v0.15.2). The left operand is the whole current syntactic slot — back to the enclosing bracket, a `,` / `;` / `:` separator, an assignment / augmented / walrus `=`, a `return` / `yield` keyword, or the line start (so `a + b as! int` casts `a + b`); the right operand is parsed as a type expression (dotted name, optional `[...]` subscript, `|`-union), so trailing code after the type (`x as! int + 1`, the `for` of a comprehension) stays outside the cast. An `as!` whose right side isn't a type expression is left for the parser to reject cleanly. (Earlier releases restricted `as!` to a single physical line in value position only.) Prefer `model X:` for a boundary you validate repeatedly, a `.dty` stub for a long-lived dependency, and `as!` for an ad-hoc one-off shape assertion.

---

## 6. v0.3.0 — annoyance-killing language features

### `newtype Name = Base` — nominal aliases over primitives

```python
newtype UserId = int
newtype PostId = int

def fetch_user(id: UserId) -> User: ...

let uid: UserId = UserId(42)
fetch_user(uid)                  # ✅
fetch_user(42)                   # ❌ tyc::newtype_violation — wrap as UserId(42)

let raw: int = uid               # ✅ asymmetric: UserId flows freely into int
```

Compiles to a zero-cost `typing.NewType` call. The relationship is asymmetric: `Newtype → Base` is free, `Base → Newtype` requires explicit construction.

**Same-newtype arithmetic preserves the newtype across `+ - * // % **`** (v0.7.0). `LogIndex(a) + LogIndex(b)` is `LogIndex`. `LogIndex(a) + 1` (literal of base) is also `LogIndex`. Two distinct newtypes with the same base (`LogIndex + Term`) fire `tyc::operator_type_mismatch`. `/` always widens to `float` (Python's true division).

Use newtypes for ID kinds, currency tags, internal-vs-external markers — anywhere "an `int` is an `int`" loses you minutes of debugging.

### `freeze let X = expr` — deep-immutable bindings

`let` only locks the binding name; `freeze let` locks the value too:

```python
freeze let CONFIG = {"port": 8080, "hosts": ["a", "b"]}

# CONFIG = {...}                 # ❌ tyc::immutable_assign (binding locked)
# CONFIG["port"] = 9000          # ❌ TypeError at runtime — MappingProxyType
# CONFIG["hosts"].append("c")    # ❌ AttributeError at runtime — tuple, not list
```

Module-level only in v1. Lowers to a `__typhon_freeze__(...)` call against `typhon_runtime.freeze.deep_freeze`, which recursively converts `list → tuple`, `dict → MappingProxyType`, `set → frozenset`, descends into nested values, and raises `TypeError` at startup on anything without a clean immutable equivalent (file handles, sockets, generators, non-frozen dataclasses). Frozen dataclasses pass through unchanged.

Stacks with `pub` (v0.6.0): `pub freeze let X = …` parses.

### `pub` — module visibility marker

```python
pub let API_VERSION: str = "v1"
pub class Client: host: str
pub def connect(host: str) -> Client: ...

let _internal_default_port: int = 8080   # not exported
```

When a module declares at least one `pub` name, desugar synthesises a top-of-file `__all__ = [...]` so `from foo import *`, Sphinx autoapi, IDE re-export filters, and the type checker's re-export inference all see the public surface. A hand-written `__all__` wins.

Stacks with every modifier: `pub class X frozen:` (the modifier is postfix), `pub model`, `pub let`, `pub mut`, `pub freeze let`, `pub newtype`, `pub interface`, `pub type`, `pub def`, `pub async def`, and `pub comptime let` / `pub comptime mut` / `pub comptime def` (v0.15.4). The one current exception is **`pub lazy let`, which does NOT parse** — see §7.

### `pub *` — package-level re-export aggregation (v0.7.0)

```python
# src/mypkg/__init__.ty
pub *
```

In an `__init__.ty`, `pub *` aggregates every direct-sibling module's `pub` names and (transitively) every direct sub-package's effective public surface. The desugar pass synthesises `from .sibling import name1, name2, ...; from .subpkg import name3, ...` at the marker and appends every aggregated name to `__all__`.

Colliding sibling names → `tyc::pub_name_collision` (names both modules and the colliding name). `pub *` outside `__init__.ty` is a no-op + `tyc::pub_star_outside_init` (advice).

See [PACKAGING.md](PACKAGING.md) for the full multi-file packaging surface.

### Three new safety/effect diagnostics

- **`tyc::blocking_in_async`** (warn) — direct call to a known-blocking stdlib (`time.sleep`, `requests.{get,post,...}`, `urllib.request.urlopen`, `subprocess.{run,call,check_call,check_output}`, `input`, `socket.recv`) inside `async def`. Suppressed inside `unsafe:`. Wrap in `asyncio.to_thread(...)` or use an async-native client.
- **`tyc::resource_not_managed`** (warn) — bare assignment of `open` / `socket.socket` / `sqlite3.connect` / `tempfile.{NamedTemporaryFile,TemporaryDirectory,TemporaryFile}` not wrapped in `with`. Severity controlled by `[strictness] require-with`. `@contextmanager` / `@asynccontextmanager` factory bodies are exempt (v0.6.0).
- **`tyc::div_by_zero_literal`** — literal divisor zero (`/ 0`, `// 0`, `% 0`, including `-0.0` and unary-negated zero) always raises `ZeroDivisionError`. Pure constant-fold; no flow analysis.

Plus `tyc::unsafe_value_leak`, `tyc::pattern_shadows_outer`, and `tyc::extend_builtin` (all from the same release).

---

## 7. Release highlights (newest first)

The v0.3.0 → v1.0.0-alpha.9 line is **additive on *correct* programs**. Every program that type-checked *and ran correctly* continues to behave identically; the new language forms in the whole window are the **`enum` keyword** (v0.11.0), the **`as!` checked boundary cast** (v0.14.0, made to compose everywhere in v0.15.0), and the **`rescue` exception-boundary sugar** (v1.0.0-alpha). Several deliberate narrowings exist, each rejecting only programs that already crashed at runtime (or relied on an unsound narrowing): v1.0.0-alpha.2's three conservative diagnostics, v1.0.0-alpha.3's four flow-narrowing invalidation fixes, v1.0.0-alpha.4's H5 scope-blind class-unification fix, and v1.0.0-alpha.7's batch of type-checker/resolver soundness closures (parametric sealed-union exhaustiveness, `let` immutability in loops and through `global`/`nonlocal`, plus one warn-level addition — nullable-field dereference). v1.0.0-alpha.5 and v1.0.0-alpha.6 added no new diagnostics — alpha.5's rewrites are opt-in or advice-only, and alpha.6's only diagnostic change is warn-level: six more secret-name keyword shapes. The behaviour changes are all in the **VM**: the v0.8.0 switch to arbitrary-precision integers, the v0.11.0 alignment of VM value semantics with CPython (value-based dataclass equality / repr / hashing, order-independent set equality, CPython-matching float repr), v1.0.0-alpha.7's `ExceptionGroup`/`except*` modelling (PEP 654), and v1.0.0-alpha.8's entropy-seeded default for an unseeded `random` — programs that relied on the old VM behaviour now compute different (correct) results. (An unseeded `random` under `tyc run` was repeatable before alpha.8 and is not now; seed explicitly if you were relying on that.) Highlights newest-first:

### v1.0.0-alpha.9 — maintenance

- **`tyc::contains_secret_literal` (warn): 16 keywords → 55.** Seven overlapping PR proposals consolidated into one longest-first-ordered table, so a name matching several keywords reports the most specific (`DB_PASSWORD`, not `PASSWORD`). Still warn-level — a newly-flagged name warns, it never fails the build.
- **One more name-word boundary.** An uppercase keyword followed directly by a lowercase letter now ends a word (`TOKENs`, `dbPASSWORDstring`), in `is_secret_name` and the `tyc build` scan alike. `MONKEY` / `PASSPORT` are unaffected — their boundary failure is on the start side.
- **Allocation reductions.** `auto_gather`'s `Candidate` (binding name, callee name, argument / keyword slices — previously cloned), `module_all_names`, and `tyc build`'s import scan borrow from the AST instead of allocating; `OpportunityCandidate` borrows its binding name, though its `deps` expressions stay owned.
- **CI.** The T0.2 differential gate now runs its harness at `--jobs 2` — at `--jobs 4` it finished with the runner's swap 99.7% exhausted. That gate's intermittent `runner has received a shutdown signal` kills are *not* explained by it (they continue at `--jobs 2`) and clear on a re-run.
- **Dependencies.** `serde_json` 1.0.151, `rustc-hash` 2.1.3, `aho-corasick` 1.1.5; `@types/node` 26.2.0 (VS Code extension). `compact_str` is held at 0.9.1 — `get-size2` 0.8 pins that version for its `GetSize` impl, and a duplicate in the graph breaks `ruff_python_ast` under its `get-size` feature.

### v1.0.0-alpha.8 — maintenance

- **VM: unseeded `random` seeds from entropy.** Previously a fixed constant, so `tyc run` was deterministic exactly where `tyc build && python` is not. Explicit `random.seed(n)` still matches CPython byte-for-byte.
- **`tyc::contains_secret_literal` (warn) widened.** Digits and UPPERCASE→TitleCase junctions now count as name-word boundaries in both `is_secret_name` and the `tyc build` scan, and `PRIVKEY` joins the keyword table (the `V`→`K` junction is not a boundary, so `SSH_PRIVKEY` never matched the bare `KEY`).
- **Allocation reductions.** `parallel_lints` and the desugarer's `collect_multi_base_parents` / `collect_cached_property_targets_into` borrow `&str` from the AST instead of collecting `String`s.
- **Dependencies.** `clap` 4.6.6, `thiserror` 2.0.20, `insta` 1.48.0, `schemars` 1.2.2, `toml` 1.1.4+spec-1.1.0; `actions/cache` 6.1.0 and `actions/setup-python` 7.0.0.

### v1.0.0-alpha.7 — codebase-review remediation

The 2026-07-28 full-codebase-review remediation release. Closes all ten of the review's 1.0 blockers plus the Tier-0 gates that make them verifiable.

- **Type checker:** instance-attribute assignment (`self.field = v`) is now type-checked; constructor field order follows reverse MRO; `model` constructors are keyword-only; recursive type aliases terminate instead of aborting; a `match` over a *parametric* sealed union is checked for exhaustiveness; a nullable **field** dereference (`self.conn.execute()`) is reported at warn level; `let` immutability is enforced inside loop bodies and through `global`/`nonlocal`; a call in any statement position (not just assign/annotated-assign/bare-call) invalidates a global narrowing; a loop body widens every channel it can carry a stale value through; a tuple unpack re-narrows its targets.
- **Preprocessor:** a shared `tyc-syntax::lexmask` module replaces ~35 hand-rolled string/comment/bracket scanners, fixing bugs the drift had been hiding — `gather:` no longer serialises a block because a binding's name appears inside a string, `with`-chain parsing no longer misreads string content as a header, and an `enum` body no longer rewrites a bracket-continuation argument as a member. `?` in an `elif`/`while` condition no longer corrupts control flow; four passes stop rewriting string-literal contents; the emitter parenthesises comparison/conditional operands and fixes a triple-quoted-string tail-quote bug.
- **VM:** `ExceptionGroup` / `except*` are modelled (PEP 654) — constructors, auto-downcast, splitting, `TaskGroup` accumulation — plus five other `except*`/string-search divergences fixed against a CPython 3.13 probe battery.
- **New gates:** a VM↔CPython differential gate over the full 1342-file example+stress corpus (first run found 126 divergences, now pinned as a baseline), an opt-in-knob codegen matrix, a post-emit parse gate, and CI now runs the Python-executing tests.

No new syntax; most changes are narrowings on code that was already crashing or relying on unsound typing, plus one warn-level diagnostic-surface addition. See `CHANGELOG.md` for the full compatibility breakdown.

### v1.0.0-alpha.6 — maintenance: dependency wave, secret-lint keywords & release-pipeline hygiene

A maintenance release on top of alpha.5, driven by the 2026-07-20 release-readiness review (`docs/release-readiness-2026-07-20.md`). **No new syntax; no previously-*correct* program changes behaviour** — the only diagnostic-surface change is warn-level.

- **Secret-name detection: six more high-risk keywords.** `API_TOKEN`, `APITOKEN`, `API_SECRET`, `APISECRET`, `API_PASSWORD`, and `APIPASSWORD` are registered in the `tyc::contains_secret_literal` keyword table and the `tyc build` secret-suffix scan, placed ahead of their shorter substrings so alpha.4's longest-first matching is preserved — squashed acronyms like `APITOKEN = "…"` no longer evade the case-boundary heuristics. The two consumers now share a single `tyc_analyse::SECRET_NAME_KEYWORDS` table, closing off the copy drift that had bitten twice before.
- **toml 1.x compatibility.** The `[dependencies]` / `[dev-dependencies]` allow-list reader in `tyc-venv` parses `typhon.toml` through the serde document path (`toml::from_str`); under the toml 1.x crate, `str::parse::<toml::Value>()` parses a single TOML *value* rather than a document, which silently emptied the allow-list (caught by a regression test during the 0.8 → 1.1 bump, before any release shipped with it).
- **Dependency refresh** (`toml` 1.1.3 major, `bitflags`, `memchr`, `bstr`; GitHub Actions majors, every pin verified against its upstream tag) and **release-pipeline hygiene** — the artifact download is re-pinned to the proven v4 line to match the v4 upload.
- **Docs & repo hygiene:** docs-site keyboard-focus / reduced-motion accessibility polish, `docs/diagnostics/README.md` indexing all 87 pages, the bundled skill's `DIAGNOSTICS.md` gaining the four v0.13.0 footgun lints it omitted, a desugar-path allocation reduction, and the `.Jules`/`.jules` case-collision fix (fresh checkouts on macOS/Windows work again).

### v1.0.0-alpha.5 — VM performance Tier 1, `[optimise]` profile, perf-advice lints & free-threading wave

A performance-focused release on top of alpha.4. **No new syntax; no previously-*correct* program changes behaviour** — every rewrite is opt-in (`[optimise]`, `auto-parallel`, `auto-parallel-reductions`, `parallel-backend`) or advice-only (the seven new lints).

- **VM performance Tier 1** (`tyc run`): `Value::Int` wraps a two-representation `VmInt` — `Small(i64)` inline, overflowing to a reference-counted `Big(BigInt)` — so common-path integer arithmetic no longer allocates, while CPython's exact arbitrary-precision semantics (floor-div/mod sign rules, `2 ** 100`, big-int↔float comparison, numeric dict-key collapse) are preserved exactly. A per-class resolved-method cache (negative results included) avoids re-walking the base chain on repeated `find_method` misses, and a direct `obj.method(args)` dispatch path skips the intermediate `BoundMethod` allocation. Functions with no `global`/`nonlocal` declarations and no captured closure variables resolve locals to a fixed slot computed once at function-definition time instead of a `HashMap` lookup per read/write. Cumulative effect on the VM performance plan's corpus: the VM's steady-state slowdown vs `tyc build` + CPython compresses from ~5–18× to ~3–14× (startup-adjusted, ~2.7–6× end-to-end); the VM still wins on startup latency. See `docs/vm-performance-plan.md`.
- **`[optimise]` config section + `tyc build -O`.** A single project-wide `level` dial: `level = 1` flips the *default* of `auto-memoise`, `auto-gather`, `auto-parallel`, and `pgo-memoise` to `true`; an explicit `[strictness]` entry for any of the four always wins. `tyc build -O` / `--optimise` applies `level = 1` for one invocation without editing `typhon.toml`.
- **New performance-advice lint family.** Six advice-level `tyc::perf_*` lints plus `tyc::lazy_import_opportunity` — all on by default, gated by `[strictness] suggest-perf`, surfaced by `tyc check` / `tyc build` / the LSP: `perf_membership_in_loop`, `perf_list_shift_in_loop`, `perf_str_concat_in_loop`, `perf_sort_in_loop`, `perf_sorted_first`, `perf_keys_membership`, and `lazy_import_opportunity`. Detectors live in `tyc-analyse/src/perf.rs`; each fires only on unambiguous local AST evidence, so the example/stress corpus stays advice-noise-free. Each code ships a `docs/diagnostics/<code>.md` page and a `tyc explain` entry.
- **Free-threading parallelisation wave** (`[python] free-threaded = true`): widened `auto-parallel` comprehension shapes (filters, multi-argument calls, nested pure calls, all semantics-preserving); a new integer accumulator-loop reduction (`[strictness] auto-parallel-reductions`, requires `auto-parallel`) that folds a provably-bounded `for x in xs: total += EXPR` (`mut total: int`, pure `EXPR`) into `total += sum(typhon_runtime.parallel.map_pure(lambda x: EXPR, xs))`; two new advice lints (`tyc::parallel_opportunity`, `tyc::shared_mut_across_tasks`); and a `parallel-backend = "interpreters"` option that tries a PEP 734 `InterpreterPoolExecutor` (3.14+) before falling back transparently to the thread pool.
- **Native PEP 810 lazy imports on Python 3.15 targets.** `3.15` and `3.15t` become accepted `[python] target` values; `tyc build` lowers `lazy import ALIAS = MODULE` to the native `lazy import MODULE as ALIAS` statement CPython 3.15 ships, instead of the `typhon_runtime.lazy.lazy_import` helper call. 3.13/3.14 output is byte-for-byte unchanged.

### v1.0.0-alpha.4 — post-alpha.3 hardening: H5 soundness, secret-detection correctness & supply-chain hygiene

A focused hardening pass on top of alpha.3. **No new syntax; no previously-*correct* program changes behaviour** — like the alpha.2 diagnostics, the H5 fix is a conservative narrowing that only rejects programs passing a provably-different-shaped class across a module boundary.

- **H5 — scope-blind class unification closed at the declaration boundary** (the one HIGH finding `RELEASE_READINESS_REVIEW.md` deferred). The v0.15.0 qualified↔bare tail-unification let a *locally declared* class satisfy a same-named class from another module (a user `class Response:` type-checked into an `httpx.Response`-typed slot, and vice versa). `is_assignable` now refuses that unification when the bare side names a class declared in the module being checked and the qualified side's declaration — resolved through its **exact** module key, never the ambiguous reverse scan that sank the first fix attempt — has a different shape. The guard is evidence-gated and degrades to the previous permissive unification on any uncertainty (unresolvable module, unknown shape, facade re-export with an equivalent shape, interfaces, bare names of unknown provenance such as provider return types), so the example/stress corpus is byte-identically unchanged. Four regression tests cover both rejection directions plus the facade-equivalence and unknown-provenance carve-outs.
- **Secret-name diagnostics match longest-first** — the keyword tables behind `contains_secret_literal` (`tyc-analyse`) and the `tyc build` secret-suffix scan ordered the bare `KEY` ahead of `APIKEY`, so a name like `KEY_APIKEY` matched the shorter `KEY` and reported the less-specific suffix. `APIKEY` now precedes `KEY`, restoring the intended longest-first heuristic.
- **Supply-chain & packaging** — every third-party GitHub Action across the workflows is SHA-pinned (Dependabot's `github-actions` ecosystem keeps the pins fresh), with a dispatch-only `normalize-release-flags` workflow to retro-flag hyphenated release tags as pre-releases; `install.sh` / `install.ps1` resolve "latest" via the release *list* (`/releases?per_page=5`, drafts filtered, pre-releases included) instead of `/releases/latest` (which excludes them), so a default install no longer silently fetches older binaries; and a round of dependency / advisory bumps lands (`crossbeam-epoch` 0.9.18 → 0.9.20 for RUSTSEC-2026-0204, `regex` 1.12.4, `memchr` 2.8.2, `compact_str` 0.9.1).

### v1.0.0-alpha.3 — release-readiness remediation: licensing, packaging & robustness

A full-codebase release-readiness review (`RELEASE_READINESS_REVIEW.md`) and the fixes it drove — the release-engineering / robustness counterpart to alpha.2's soundness sweep. **No new syntax; no previously-*correct* program changes behaviour** (the four flow-narrowing fixes are conservative widenings that only affect programs relying on a previously-unsound narrowing).

- **Licensing & packaging** close the gaps that would block a clean public release: a repository-root MIT `LICENSE`, the upstream Ruff MIT notice vendored beside the fork (`tyc/vendor/LICENSE`), `SECURITY.md` / `CONTRIBUTING.md` / `.github/dependabot.yml`, and release-workflow hygiene — pre-release tags (a `-` in the tag) publish with `prerelease: true`, and auto-tag only fires after CI succeeds on the commit (`workflow_run` gate) so an untested merge can't auto-publish binaries.
- **Type checker** — three reporting / complexity fixes: identical errors at *distinct* source locations are all reported now (the module- and resolver-level dedupe keyed too coarsely and swallowed the 2nd-and-later occurrence; the B24 sealed-union `impl`-distribution collapse is preserved by remapping the synthetic copy's spans before keying); nested-generic assignability is linear again instead of O(2^depth) (a single-pass `types_equivalent` replaces `assignable(a,b) && assignable(b,a)` — a depth-28 check drops 5.4 s → 7 ms); and four more flow-narrowing invalidation holes are closed (an `except` handler is checked with narrowings widened to declared types; a loop-reassigned variable is widened before the body; an assignment-RHS call invalidates a `global` narrowing; a bare method-call statement invalidates attribute narrowings rooted at its receiver).
- **VM ↔ CPython parity** (six gaps): cyclic-value `==` / `<` no longer overflows the native stack (depth-guarded, with an `Rc::ptr_eq` identity fast-path); `str.find` / `rfind` / `index` / `rindex` return **character** offsets, not byte offsets; augmented assignment on a list mutates in place (`b = a; b += [x]`); float `%` follows the divisor's sign and float `//` / `%` by `0.0` raise `ZeroDivisionError`; `print` tolerates a broken pipe (`tyc run app | head`); and `json.dumps` coerces scalar dict keys to strings.
- **Tooling** — the LSP reserves a 256 MiB stack (matching the CLI); `tyc fmt` writes atomically (temp-file + rename); **`TYC_NO_INTROSPECT`** disables venv dependency introspection in the CLI and LSP (a kill-switch for the "opening a project imports its dependencies" trust boundary, documented in `SECURITY.md`); Windows venv discovery probes `.venv\Scripts\python.exe` and `python` / `py`; and **`[strictness] exhaustive-match` is now actually applied** — `"warn"` demotes `tyc::non_exhaustive_match` and `"off"` drops it (previously the validated knob silently did nothing).
- **Diagnostics / docs** — diagnostic doc URLs point at resolvable GitHub paths (`github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/<code>.md`) instead of the never-deployed `typhon.dev`; four diagnostics gained doc pages + `tyc explain` entries (`empty_collection_no_annotation`, `freeze_not_freezable`, `newtype_invalid_base`, `typing_alias_in_annotation`), completing the catalog.

### v1.0.0-alpha.2 — type-checker soundness sweep + VM parity

Remediation of a 2026-06-28 adversarial pre-release review, and the first release to deliberately narrow the accepted surface — but only for programs that already crashed at runtime. **No new syntax; no previously-*correct* program changes behaviour.**

- **Soundness:** flow narrowing of a **non-local** place (a global or instance field) is invalidated across an intervening call or alias write (a narrowed `self.val` no longer survives a method that nulls it, then crashes at runtime); short-circuit narrowing (`x is not None and x.method()`, and the De Morgan `x is None or x.method()`) no longer false-positives on the canonical Python null-check idiom.
- **Newly-typed positions:** slice reads, subscript assignments, `for` / `let` tuple-unpack targets, `match` star / or-pattern captures, walrus bindings, and parameter defaults are all typed and checked instead of degrading to `Unknown`.
- **Three conservative diagnostics** — `tyc::not_a_context_manager` (a `with` over a local class lacking the protocol), `tyc::raise_non_exception` (`raise 42` / raising a plain dataclass), `tyc::frozen_inheritance_conflict` (mixed frozen / non-frozen dataclass bases) — each firing only where the emitted Python already crashed.
- **VM ↔ CPython parity:** `bytes %`-formatting, numeric int / float / bool dict-key collapse, and VM `as!` enforcement (a wrong cast no longer slips through the VM).
- **Robustness:** `typhon.toml` rejects unknown keys and invalid `[checker] external` / `[strictness]` severities (instead of silently reverting to defaults); the CLI runs on a 256 MiB worker stack so deeply-nested or very long expressions no longer abort.

### v1.0.0-alpha — first feature-complete alpha

Typhon's first tagged alpha and first *feature-complete* milestone: the proven production surface plus the previously-deferred type-system frontier. Rolls up milestones M1 + M2 of the alpha release plan, the early M3 polish (formatter idempotence, the perf-regression CI gate), and the **`rescue`** exception-boundary sugar (see [§9](#9-error-handling-with-resultt-e)). Frontier work landed:

- **Higher-kinded type unification** — a constructor variable `F` in `class Functor[F[_]]:` binds against a concrete head like `list`, with `tyc::kind_mismatch` on wrong arity / conflicting binding. (Function-level `F[_]` params remain deferred — see `TYPE_SYSTEM_FRONTIER.md`.)
- **User-generic variance inference** — covariant / contravariant type-params inferred from usage, across module boundaries, with `@covariant` / `@contravariant` overrides; variance flows through generic interface bounds.
- **General inter-procedural field-init audit** (partial instances tracked across helper chains) and **2-member non-nullable union modelling** at the introspection boundary.

The production path (`tyc build` → CPython 3.13+) is stable and carries no runtime dependency on the toolchain. As an **alpha** the surface syntax is *not yet frozen* and may change before `1.0.0` with a documented migration note. Deferred to beta: embedded `ty` Phase 2 (the Phase 1 subprocess path ships), typeshed-backed pure-extension checking, and the function-level HKT tail. Additive on the accepted surface — every previously-accepted program type-checks identically.

### v0.15.7 — third-party introspection depth + stress-round robustness

Deepens compile-time checking of third-party code and clears a batch of false positives (2026-06-21 stress round + a 43-library introspection audit). **Third-party *method* calls are now arity-checked** — a missing required argument to `PCA(n_components=2).fit()` (sklearn `fit(self, X, y=None)`) or `df.merge()` is caught at `tyc check` / `tyc build` time, the way constructor and free-function calls already were. Introspection-robustness fixes remove build-blockers on valid code: a re-exported proxy that raises from `inspect.signature` (Flask's `current_app`, Django's `settings`) no longer disables a whole module's checks; the implicit-Optional `x: T = None` idiom no longer false-positives; `pkg.sub.Thing()` multi-segment attribute calls are checked. Three pure type-checker false positives fixed (`Counter + Counter` / `Counter - Counter`, a `plain class` / `class!` with a hand-written `__init__`, a `__call__`-bearing instance passed where a `Callable` is expected) plus a VM `str.isupper()` / `islower()` parity gap.

### v0.15.6 — stress-test robustness sweep

A ~198-program stress-robustness sweep clearing a cluster of type-checker false positives: custom exception classes (`class FooError(Exception):` is no longer auto-`@dataclass`'d, so `raise FooError("msg")` works), nested / list-star / tuple-of-union `match` exhaustiveness, `isinstance`-narrowed bare containers usable parametrically, iterator-protocol classes conforming to `Iterator[T]`, and `dict`-view set operations — plus VM parity gaps (`**kwargs` order, slice deletion, `__format__` dispatch, `bytes` operators, exception `str` / `repr`). Two small additive features: flow-sensitive attribute narrowing (`if self.x is None: return …`) and a VM `abc` module shim.

### v0.15.5 — cross-module `extend BUILTIN:` propagation

`extend BUILTIN:` now crosses module boundaries end-to-end (type checker, build / codegen, and VM runtime): importing a module that declares `extend str: def slug(...)` makes `title.slug()` resolve in the consumer. Previously module-local — the free-function workaround (`pub def to_slug(s: str) -> str: …`) still works but is no longer necessary.

### v0.15.4 — cross-module interface conformance + `pub comptime let`

Closes the cross-module structural-interface-conformance gap: a concrete class reaching a consumer only via an imported provider's return type or a `mod.Iface`-qualified annotation now conforms (the checker resolves class / interface shapes through the project-wide registry, not just directly-imported names; a non-conforming concrete still errors). Also fixes a `pub comptime let` / `pub comptime def` parse error — `pub` now stacks with `comptime`.

### v0.15.3 — `tyc install skill` + bundled-skill refresh

A tooling release with no language, type-checker, VM, or emitted-runtime change: the `typhon` Claude skill now ships embedded in the compiler, and `tyc install skill` vendors it (with its `references/` examples) into any project's `.claude/skills/typhon/`. The bundled skill was brought current with the v0.14.1 → v0.15.2 surface.

### v0.15.2 — `as!` comment-awareness fix

A bugfix with no language/API change. The `as!` checked-cast preprocessor built its string/comment skip mask in two passes (string-first, comment-blind), so an apostrophe inside a `#` comment (`# assert each field's shape`) opened a *phantom* string that swallowed an `as!` / `?` on a following line and surfaced as a spurious `tyc::parse` error. Multi-byte characters in such a comment (em-dash `—`, bullet `•`) shifted byte offsets the same way. The mask is now built in a single unified pass — a `#` outside a string starts a comment to end-of-line, and quotes inside it are inert.

### v0.15.1 — compiler performance + docs-site accessibility

No language/API change. **Source-map generation is now O(N log N)** instead of O(N²): `build_source_map_v2` precomputes newline positions once and resolves each token offset with a binary search (`partition_point`) — a 10,000-line file drops from ~31 s to ~64 ms. The type-checker's `cases_cover_type` no longer heap-allocates the `Ok`/`Err` variant names per call. Docs-site gains a visible keyboard focus ring on scrollable code blocks and a brief heading highlight on anchor navigation.

### v0.15.0 — `as!` everywhere, `try_result`, compiler-bundled library stubs

Driven by a field report from building a real async app — four threads at the library boundary.

- **`as!` composes in any expression position.** The checked cast now lowers **structurally** (a fixpoint rewrite, bracket-/string-/comment-aware) instead of line-by-line, so it works nested in call arguments (`save(row[0] as! int, label)`), inside comprehensions / collection literals, across a multi-line value expression, and in statement conditions (`if raw as! bool:`). The left operand is the current syntactic slot (back to an enclosing bracket, a top-level `,`/`;`/`:`, an assignment, a `return`/`yield`/`if`/`while`/`assert` keyword, or line start); the right operand is parsed as a type expression. The VM intercepts `__typhon_checked_cast__` before argument evaluation so a union / parametric target (`x as! int | None`, `d as! dict[str, int]`) runs under `tyc run`. See [§5.10](#510-as--checked-boundary-cast-v0140).
- **`try_result(thunk[, on_err])`** — a **prelude name** (no import in source, like `Ok`/`Err`) typed as `Result[T, E]`. Runs `thunk()` → `Ok(result)`; on any exception → `Err(on_err(exc))`, or `Err(exc)` (raw exception) when the mapper is omitted. `T` from the thunk body, `E` from the mapper body. Special-cased in `infer_expr` like `as!`, so a wrong return annotation still fires `type_mismatch`. `tyc build` auto-injects `from typhon_runtime import try_result`; the VM registers it as a prelude native. See [§9](#9-error-handling-with-resultt-e).
- **Compiler-bundled `.dty` stubs** (httpx, requests to start) ship embedded in `tyc` and are seeded into the project shape map by `tyc check` / `tyc build` / the LSP **before** venv enrichment — so an imported bundled library is shaped out of the box, type-checked, and its `unintrospectable-dependency` warning suppressed, with no `.venv` or `tyc sync`. Gap-fill, not override: an authored project `.dty`/`.ty` wins; the bundle beats venv introspection. Lives in `tyc-db::seed_bundled_stubs` (`.dty` text under `tyc-db/src/bundled/`).
- **`async_without_await` understands async contracts** — an awaitless `async def` is no longer warned when it's async only to implement an async `interface` method or override an async base method (removing the dead `await asyncio.sleep(0)` no-ops the diagnostic used to force). An `async` impl of a *sync* method still warns.
- **Qualified cross-module class references unify with their bare form** — `import httpx; let r: httpx.Response = client.get(...)` no longer mismatches the method's bare `Response` return. `is_assignable` unifies two class types whose final `.`-segments match **when at least one side is bare**; two different *qualified* classes stay distinct (`httpx.Response` ≠ `requests.Response`).

### v0.14.3 — LSP: live config refresh

The language server now handles `workspace/didChangeWatchedFiles`, so a `typhon.toml` edit (e.g. toggling `[strictness] suggest-gather`) invalidates the per-project config cache (keyed by mtime) and re-checks every open document immediately. Two committed end-to-end `#[tokio::test]`s drive the real server over an in-memory pipe.

### v0.14.2 — async concurrency advice + cross-module auto-gather

- **`tyc::gather_opportunity`** (advice, **on by default**) — `tyc check` / `tyc build` flag every run of 2+ adjacent independent awaited calls inside an `async def` and suggest wrapping them in an explicit `gather:`. Unlike `auto_gather_missed`, it is **callee-agnostic**: it fires for awaited **method calls on imported clients** (`await client.get_user(...)`), the shape `auto-gather` never touches. Independence is decided by static data flow (a later await referencing an earlier-bound name — including through the callee's receiver — ends the run); it **never rewrites** (concurrency is an opt-in behaviour change) and is advice-level (never blocks a build). Knob: `[strictness] suggest-gather` (default `true`). `tyc explain gather_opportunity` documents it offline.
- **`auto-gather` now folds imported `@gatherable` callees** — a run of independent awaits whose callees are `@gatherable` async functions imported from another project module folds into an `asyncio.TaskGroup`, exactly like a same-module run. Each module's `@gatherable` set is published on `ModuleShapes`; the `@gatherable` decorator is still required, so the safety boundary holds across modules (mis-resolution can only *fail* to fold, never fold something un-attested).
- **Advisory lints surface live in the editor.** The pure-AST advisories (`gather_opportunity`, `mutable_default_param`, `empty_collection_no_annotation`, `typing_alias_in_annotation`, `is`-literal, loop-closure-capture, inline secret-literal) now flow through the LSP via a single shared `editor_lint_diagnostics`, rendered as unobtrusive hints. No VS Code extension change needed.

### v0.14.1 — cross-module shape propagation completeness

A dogfooding round surfaced four per-module checker tables never threaded through the cross-module shape registry — `newtypes`, transparent `type_aliases`, `enums`, `frozen_classes` — now all carried through the same path that already propagated `sealed_unions` / `interfaces` / `class_shapes`. Fixes (all soundness/false-positive): an imported `newtype` now widens to its base (and a bare base into an imported-newtype slot surfaces `newtype_violation`, not a generic mismatch); an imported transparent `type` alias unwraps in the consumer; an exhaustive `match` over an imported `enum` no longer false-fires `missing_return`; and a field write on an *imported* `frozen` class now correctly trips `tyc::frozen_assign` (was silently accepted → `FrozenInstanceError` at runtime). Additive — every previously-accepted program type-checks identically.

### v0.12.0 — VM `__lt__` parity, dict/str builtins, deep library introspection

The headline is **deep compile-time library introspection** plus a VM comparison-protocol fix. See `CHANGELOG.md` for the full list.

**Third-party argument-*type* checking (annotation capture):**

- **Venv introspection captures parameter and return *annotations*** (previously only name / kind / has-default, so every third-party param was `Type::Unknown` and only *arity* was checked). It maps the unambiguous scalar builtins (`int` / `str` / `bool` / `float` / `bytes` / `None`), nullable `Optional[X]` / `X | None`, parametric containers (`list[X]` / `set[X]` / `frozenset[X]` / `dict[K, V]`), and fixed-arity `tuple[...]` (recursively; an unresolvable element degrades to a permissive `Unknown`). A fully-typed pure-Python dependency now gets argument-*type* checking for **free-function and constructor** calls through the same `tyc::type_mismatch` machinery your own code uses — e.g. a dependency exposing `def fetch(url: str, ...)` called as `fetch(12345)`, or constructing a `Client(host: str, port: int)` with `port="oops"`. The dependency must ship **inline** annotations for this to fire — a stub-only library like `requests` (typed via typeshed's `types-requests`, not in its own source) degrades to `Unknown` under venv introspection and is instead caught by Layer 3 (`ty`/typeshed) or a `.dty` stub.
- **In-project constructor arguments are now type-checked too** (closes a soundness hole): `check_concrete_constructor_args` validates each positional / keyword argument against its concrete field type. Too-many-positional is caught for zero-field constructors when the shape is authoritative; `plain class` / `class!` are exempt (they may carry a hand-written `__init__`).
- **Conservative by design:** anything not confidently modelled degrades to `Unknown`, so the change can only *add* true positives. Verified across the 256-file corpus with zero false positives.
- **`[strictness] unintrospectable-dependency`** (default `"warn"`) — a declared dependency that's imported but can't be introspected (no reachable `.venv` / `python3`, not installed, or no introspectable signatures) now *warns* instead of silently skipping its third-party checks. `"error"` CI-gates; `"off"` restores the old silent behaviour.
- **Live in the editor:** `tyc lsp` now runs the venv introspection (persistent per-project `VenvSignatures` cache, invalidated on `.venv/pyvenv.cfg` mtime change), so wrong-typed / wrong-arity third-party calls squiggle as you type. The introspection logic moved into a new shared crate **`tyc-venv`** (was private to the `tyc` binary), consumed by both the binary and `tyc-lsp`.

**`ty` typeshed integration (Phase 1):**

- **`[checker] external = "ty"`** (default `"none"`) runs Astral's `ty` over the emitted Python after a successful build, the only path that type-checks against **typeshed** (covers C-extension and stdlib APIs venv introspection can't see — `os.path.join(1, 2)` now caught). `ty` errors fail the build. `[checker] external-args = [...]` forwards extra flags. Requires `ty` on `PATH`.
- **`--with-ty`** on `tyc build` / `tyc check` runs the `ty` pass for one invocation without editing `typhon.toml` (`tyc check --with-ty` builds to a throwaway dir first). The shared `run_ty_check` helper is factored out of the `tyc ty` command so the standalone command and the build hook share one path. Diagnostics are re-attributed to `.ty` via `.py.map`.
- **Phase 2 (embedded, in-process `ty`) is prototyped and proven feasible but NOT shipped** — it needs a git dependency on `astral-sh/ruff`, which the repo's `cargo deny` policy (`[sources] unknown-git = "deny"`) disallows, and it offers no capability the subprocess path lacks. Correction worth recording: the earlier claim that Phase 2 was blocked by the vendored Ruff fork is *false* — vendored `ruff_python_ast` (`0.0.0-typhon-vendor`) and `ty`'s upstream (`0.0.0`) coexist, kept apart by version; the real gate is the git-source supply-chain policy.

**VM / CPython parity:**

- **`sorted()` / `min()` / `max()` honour a user `__lt__`** — they routed through the dunder-blind `Value::py_cmp` (which calls class instances "equal"), so `sorted([R(1), R(3), R(2)])` returned the list *unsorted*. They now use `Interpreter::value_cmp` (the path `list.sort()` already used). Silent-wrong-output fix.
- **`str.replace(old, new, count)`** honours the third arg; **`sorted(..., reverse=True)` / `list.sort(reverse=True)`** are stable (reverse the comparator, not the list); **`json.dumps(sort_keys=True)`** sorts; **`math.isnan` / `isinf` / `isfinite`** added; float-presentation format types (`e`/`f`/`g`/…) coerce int/bool operands.
- **New builtins:** `dict.popitem()`, `dict.fromkeys(iterable[, value])`, `str.maketrans(...)`, `str.translate(table)`.

**Resolver:** dict comprehensions now bind tuple-unpack targets (`{k: v for k, v in d.items()}`).

### v0.11.0 — VM parity sweep + `enum` keyword

Closes 22 findings from an adversarial stress round against v0.10.0 (almost all in the VM).

- **`enum` keyword** — first-class declaration sugaring over `enum.Enum` (see [§5.7](#57-class-vs-model-vs-plain-class-vs-class-vs-enum)). Bare members auto-`enum.auto()`; explicit values preserved; `tyc fmt` round-trips.
- **`Value::Complex(f64, f64)`** is a real VM value — `complex(...)` / complex literals construct it, arithmetic promotes across int / float, reflected dunders dispatch, hashable for dict / set keys.
- **`Value::DictView`** backs `dict.keys()` / `.values()` / `.items()` — they repr as `dict_keys([...])`, iterate, support `len`, are `in`-testable, and re-iterable (previously each materialised a fresh list, so `d.keys() == d.keys()` was identity-false).
- **Bare `super()` rewritten to two-arg `super(EnclosingClass, self)`** in `tyc-desugar` (the zero-arg form crashed under `@dataclass(slots=True)`, which orphans the `__class__` cell). Explicit `super(X, y)` is left untouched.
- **`__call__` dispatch on callable instances; `__post_init__` invoked after auto-generated construction; multi-level inheritance accumulates fields across the full MRO.** Generic operator handler reaches every numeric / bitwise / matmul slot with reflected fallback; subscript `__missing__` fires (backs `defaultdict`).
- **VM stdlib expansion:** `collections.defaultdict(factory)`, `datetime` shim (naïve / UTC), `pathlib` shim (`/` join, `.parent` / `.name` / `.stem` / `.suffix` / `.suffixes` / `.parts`), `bytes` methods, `itertools.groupby(key=)`, real `re.Match.group(n)` / `.groups()` / `.groupdict()`, `str.split(maxsplit=)`, f-string `{x=}` debug, `str %` / f-string `%` runtime formatting, banker's-rounding `round`.
- **VM value semantics now match CPython** (silent-wrong fixes): value-based dataclass equality **keyed on class identity** (distinct same-named classes from different modules no longer collide), `Name(field=value, ...)` repr, hashable instances (`HashKey::Instance`), order-independent set / frozenset equality, canonical-sorted set repr, CPython-matching float `repr` (shortest round-trip).
- **Type checker tightening:** `None` flows into `object` (`def log(msg: object = None)` now accepted); `str %` is type-checked; `(5).items()` / `5["a"]` / `for x in 5:` fire at check time instead of crashing at run time.
- **`tyc init` seeds `allow-secret-comptime = false`** in the generated `typhon.toml`.

### v0.10.0 — VM completeness release

Closes the dunder-dispatch and builtins-coverage gaps that stopped `tyc run` from being a drop-in for `tyc build && python` on real-world programs.

- **Operator overloading on instances** — every numeric / bitwise / matmul dunder plus reflected forms; **rich comparisons** (`__eq__` / `__ne__` / `__lt__` / …); `in` / `list.index` / `.count` / `.remove` use `__eq__`-aware comparison. **`__str__` / `__repr__`** honoured by `print` / `str` / `repr` / f-strings; `__len__` / `__getitem__` / `__contains__` dispatch.
- **`@property` getters** fire on attribute read; **`@classmethod`** binds `cls`; both inherited (and the descriptor marker is cleared on override).
- **VM generator support** — `yield` / `yield from` work, materialised **eagerly** (a tree-walk can't suspend a frame; `Rc` values aren't `Send`). `GENERATOR_CAP = 1_000_000` bounds `while True: yield` to a clear `RuntimeError`. Lazy / unbounded generators and `@contextmanager` generators inside `with` still need `tyc build`.
- **`type(x)` is a real type object** — `type(x).__name__`, `type(x) == int`, `str(type(x))` → `<class 'int'>` all work (previously `type(x)` returned a plain string).
- **VM pydantic `model_validate` / `model_dump` / `model_dump_json`** make flat `model` classes usable under `tyc run` (nested-model validation still needs `tyc build`).
- **Keyword args to builtin methods** via a kwargs sentinel — `max()` / `min()` take `key=` / `default=`; `list.sort()` takes `reverse=` / `key=`.
- **Long tail of builtins** — `divmod`, `pow` (2- and 3-arg), `format`, `ascii`, `int(str, base)` (incl. `base=0`), full set algebra, the missing string methods (`center` / `ljust` / `rjust` / `zfill` / `partition` / `removeprefix` / `expandtabs` / …, and `strip`/`lstrip`/`rstrip` now honour their `chars` arg), `dict(other)` / `dict(**kwargs)`, `math.gcd` / `lcm` / `factorial` / `isqrt` / `comb` / `perm`, `json.dumps(indent=…)`, `time.perf_counter` / `process_time` (and `monotonic` fixed).
- **Type-checker false-positive fixes:** exhaustive `match` over `bool` / string-literal unions / irrefutable fixed-arity tuples no longer fires `missing_return`; augmented assignment on scalar targets (`s += 5`) is type-checked.
- **`allow-secret-comptime` strictness knob wired through** (was documented but never threaded into the analysis); `tyc-emit` literal-emission hot path de-allocated (`itoa` / `ryu`).

### v0.9.2 / v0.9.1 — bugfix point releases

- **v0.9.2** — cross-module `tyc::attribute_not_found` false-positive on a `class! Sub(Foreign):` (e.g. `class! HttpError(Exception):`) declared in one module and imported by another. The fix seeds `class_parents` from `InterfaceShape.bases` during cross-module shape ingestion so the four hierarchy walkers see the same parent chain across module boundaries. No language / runtime / diagnostic-surface change.
- **v0.9.1** — four `tyc fmt` round-trip corruption modes (`impl Alias:` for sealed-union aliases, the `frozen` modifier, standalone `pub *` lines, multi-line kwarg `=` respacing — the last one could emit a *silently-empty* file) plus a `pub *`-facade hole where an exhaustive `match` over a facade-imported variant fired `missing_return`. Scoped to `tyc-format` and the shape-map plumbing. No language / runtime / diagnostic-surface change.

### v0.9.0 — stress-test cleanup release

The big v0.8.1 → v0.9.0 sweep. Closes 32 of 36 findings from a v0.8.1 stress sweep. The VM now runs the surface the docs always advertised; the type checker plugs silent correctness gaps in covariance, variant flow, narrowing, and error propagation. See `CHANGELOG.md` for the full list.

**VM coverage** (closing the gap between `tyc run` and `tyc build && python build/main.py`):

- **`Result` combinators** (`.map` / `.map_err` / `.and_then` / `.or_else`) work on `Ok` / `Err` values in the VM via bound `NativeFn` wrappers (v0.9.0). Previously a typecheck-clean program crashed at run-time with `AttributeError: Ok has no attribute 'and_then'`.
- **`open()` write / append / binary modes** (v0.9.0). `open(p, "w")` / `open(p, "a")` / `open(p, "wb")` / `open(p, "r+")` and friends work. `with`-blocks honour `__enter__` / `__exit__`. `json.load` / `json.dump` ride on top.
- **Match against built-in class patterns** (v0.9.0). `match x: case str() as s:` / `case int() as n:` / etc. match; exhaustiveness recognises `case None:` + `case str() as s:` as covering `str?`.
- **`frozenset(...)` hashable as a dict key** (v0.9.0) — new `HashKey::FrozenSet` variant with insertion-order-independent hashing.
- **f-string `_` thousands separator** (v0.9.0) emits the same way `,` does.
- **`bytes` repr matches CPython** (v0.9.0) — `b'hi'` by default, `b"with 'embedded'"` fallback, `\xNN` for non-printable bytes.
- **Native shims for `collections.deque`, `heapq`, `contextlib`, `pydantic`** (v0.9.0). Graph / queue / heap algorithms, `@contextmanager` identity decorators, and `model` class declarations all run cleanly. `deque` rides on `Value::List` via new `popleft` / `appendleft` / `extendleft` / `rotate` list methods. `pydantic.BaseModel` is a placeholder.
- **`@property` / `@classmethod` / `@staticmethod` / `super()`** builtins (v0.9.0) — identity-ish stubs so decorated methods don't crash on import.
- **`lazy import np = numpy`** (v0.9.0) uses the simpler `import M as N` rewrite in VM mode (the descriptor-based proxy class the build path emits has nothing to bind against in a tree-walking VM).
- **Multi-file projects** under both `tyc run` modes (v0.9.0). The VM loads sibling `.ty` modules from the project source root, honours relative imports (`from .repo import x`), and caches each module's bindings as a `Value::Module`. `tyc run --compile` spawns `python -m <pkg>.main` instead of `python build/main.py` so relative imports in the entry point resolve correctly.
- **`dataclasses.field(default_factory=list)` invokes the factory per instance** (v0.9.0). `tags: list[str] = []` no longer shares one list across every instance.
- **`class!` synthesised `__init__` runs** (v0.9.0). `except HttpError as e: print(e.code)` works against `class! HttpError(Exception): code: int; message: str` — the handler binds the user `Instance`, and exception-type matching walks the MRO.
- **`freeze let CFG = {...}` actually freezes** (v0.9.0): list → tuple, dict → mappingproxy-tagged dict, recursive. Mutators on a frozen dict raise the same `TypeError` CPython's `MappingProxy` does.
- **`comptime let X = ...` inlines in the VM** (v0.9.0) via the substitution pass shared with `tyc build`. `comptime let PORT = int(env(...))` no longer crashes with `NameError: env is not defined`.
- **Typed tuple unpack `let (a: int, b: str) = pair()` parses in the VM** (v0.9.0; parity with `tyc check`).

**Type checker** (silent-correctness gaps):

- **Read-view covariance** (v0.9.0). `list[Subclass]` / `tuple[Subclass]` / `set[Subclass]` / `frozenset[Subclass]` flow into `Sequence[Super]` / `Iterable[Super]` / `Iterator[Super]` / `Collection[Super]` / `Container[Super]` / `Reversible[Super]`. Mapping / MutableMapping cover `dict[K, V]` (K invariant, V covariant).
- **Variant → parametric sealed union assignability** (v0.9.0). `Cons[T]` / `Cons` (where `type LL[T] = Cons[T] | Nil`) is assignable into `LL[T]`. Required for recursive ADT walks like `mut cur: LL[T] = self`.
- **`while True:` reachability** (v0.9.0). A loop whose body always returns/raises with no `break` is recognised as exiting; the post-loop point is unreachable and `missing_return` doesn't fire.
- **Post-while-loop narrowing** (v0.9.0). After `while y is None: y = load()` (no `break`), `y` is narrowed to non-None after the loop.
- **`assert x is not None` narrows** (v0.9.0) — the standard Python static-checker idiom works.
- **`*args` / `**kwargs` require annotations** (v0.9.0; Rule 1). Canonical idiom is `*args: object` / `**kwargs: object`.
- **`extend list:` dispatches on `list[T]`-annotated receivers** (v0.9.0). The synthetic `__typhon_builtin_ext_list` class shape is consulted before `attribute_not_found` fires.
- **Exhaustive `match` on `T?` recognises built-in class patterns** (v0.9.0). `case None: ...; case str() as s: ...` against `str?` no longer surfaces `missing_return`.
- **`with`-chain explicit `else err: return Err(err)` validates the error type** (v0.9.0). Previously the check was gated on the synthetic `?`-op temp shape, so a `with`-chain could silently return the wrong error class.
- **`func[T](args)` explicit type instantiation** (v0.9.0) fires a clear check-time error instead of crashing at runtime with `'function' object is not subscriptable`.
- **`comptime let T: type = int`** (v0.9.0) lowers to a PEP 695 `type T = int` alias so `T` is substitutable wherever a type is expected. `tyc check` runs the substitution before parsing the resolved module so check, build, and VM all see the same shape.
- **`freeze let X = <expr>` validates freezability at check time** (v0.9.0). New `tyc::freeze_not_freezable` fires when the RHS constructs a non-`frozen` user class, instead of letting the failure surface as a runtime `TypeError` at first import.
- **`pub *` name collisions surface in `tyc check`** (v0.9.0). The detection logic from `tyc build` is exposed as `detect_pub_star_diagnostics` and called from the check command, so CI catches collisions before they reach build.

**Diagnostics polish** (v0.9.0):

- **`interface_not_conforming` arity message** reads "got N non-self parameter(s), expected M" instead of the ambiguous "arity N; expected M".
- **`invalid_question_op` help text** mentions both the Result-return cause AND the comprehension carve-out.
- **Sealed-union impl distribution dedupes** diagnostics by `(code, rendered message)` so a 10-variant union no longer reports 10 identical errors.
- **`class_attr_shadows_slot` no longer false-positives** on classes whose only annotated defaults are mutable literals (`list[str] = []` etc.). Those become `default_factory` per-instance fields.
- **`MissingAnnotation` text** drops the double-backtick wrapping (was `` `parameter `x`` ``).

**Docs** (v0.9.0):

- Cheat sheet documents `class X frozen(Base):` (the modifier comes BETWEEN the class name and the base list, NOT after `(Base)`) and the `*args: object` / `**kwargs: object` idiom for genuinely variadic functions.

**Known limitations carried forward**:

- Preprocess line-number leakage (B15) — diagnostics still report preprocessed-buffer line numbers for `impl Alias:` distribution over sealed unions. The dedupe pass cuts the *count* of noise diagnostics but each surviving diagnostic still points at a synthetic line index past EOF of the original source.

### v0.8.1 — bugfix point release

- **`tyc::attribute_not_found` no longer fires on venv-introspected third-party classes** (v0.8.1). The v0.8.0 firing-site widening trusted shapes built by `inspect.signature(Cls)` to be method-complete, so `obj.method(...)` against any third-party class with a known `__init__` flagged the call as missing the attribute (`uvicorn.Server.serve(...)`, `httpx.AsyncClient.aclose(...)`, `fastapi.Request.body(...)`). `InterfaceShape` now carries a `partial` flag set on every venv-derived shape; `class_hierarchy_fully_known` returns `false` whenever any class in the chain is partial. Strictly a narrowing of the diagnostic; no language, runtime, or stdlib changes.

### v0.8.0 — stress-test sweep

- **`tyc::attribute_not_found` fires on class instances and generic classes** (v0.8.0) — not just `TypeVar`-bounded parameters. Foreign / venv-introspected classes carry a `partial` shape marker and stay lenient (so `uvicorn.Server.serve(...)`, `httpx.AsyncClient.aclose(...)`, `fastapi.Request.body(...)` don't false-positive). Skipped in `unsafe:` and on dunder / underscore names.
- **Interface parameter type conformance** (v0.8.0) — `interface_missing_members` compares param types position-by-position (contravariant) in addition to arity.
- **`Type::LitStr(String)` — string-literal singleton types** (v0.8.0) — `type Color = "red" | "green" | "blue"` and `Literal["a", "b"]` produce `LitStr` slots. Bidirectional inference widens string literals to `LitStr` only when the expected type carries one.
- **`?` propagation inside `with`-chains** (v0.8.0) — `result_error_mismatch` fires when the implicit return form of `with x = f()?: …` routes a mismatching error type.
- **`tyc::pattern_shadows_outer`** (v0.8.0) — fires when a `match` capture binds a name already in the outer scope.
- **`newtype Foo = "literal"` is rejected** (v0.8.0) — new `tyc::newtype_invalid_base` diagnostic.
- **`field_default_ordering` skips `ClassVar` fields** (v0.8.0).
- **Exhaustive-match-with-guards no longer fires `missing_return`** (v0.8.0) when every variant has at least one (possibly-guarded) case.
- **Function parameter rebinding requires `mut`** (v0.8.0) — matching the `let`/`mut` rule everywhere else.
- **VM: arbitrary-precision integers** (v0.8.0) — `Value::Int` is now `num_bigint::BigInt`. `2 ** 100` and `fib(99)` no longer overflow. **Behaviour change.**
- **VM: insertion-ordered dicts** (v0.8.0) — `RcDict` is now `indexmap::IndexMap`. Same `.ty` no longer prints dicts in different orders under `tyc run` vs `tyc build && python build/main.py`.
- **VM: f-string format flags fully wired** (v0.8.0) — zero-pad, alternate-form, `[fill]align`, sign, width, comma, precision, type all match CPython.
- **VM: mapping match patterns + sequence-with-star patterns** (v0.8.0) — `case {"k": v}`, `case {…, **rest}`, `case [x, *rest, y]`.
- **VM: recursion limit raised to 1000** (v0.8.0) to match CPython.
- **VM: larger native stdlib** (v0.8.0) — `re`, `typing`, `collections` (`OrderedDict`, `defaultdict`, `Counter`, `namedtuple`), `functools` (`lru_cache`, `cache`, `cached_property`, `reduce`, `partial`), `itertools` (`chain`, `count`, `cycle`, `accumulate`, `combinations`, `permutations`, `product`, `islice`, `takewhile`, `dropwhile`, `groupby`), `dataclasses`, `pathlib`.
- **VM: subclass constructors inherit fields** (v0.8.0) — `class Dog(Animal): breed: str` accepts `Dog(name=…, age=…, breed=…)` under `tyc run`.
- **VM: `yield` / `async def` emit a clear `NotImplementedError`** (v0.8.0) pointing at `tyc build && python` as the fallback (instead of crashing the interpreter).
- **Parser scaffolds for advertised forms** (v0.8.0): HKT `class Functor[F[_]]:`, `impl[T] SealedUnionAlias[T]:` distributing methods across every variant, generic-plus-frozen `class X[T] frozen:`, `async def` in `interface` bodies auto-completing the body, outer-annotation tuple unpack (`let (a, b): tuple[int, str] = …`).
- **Synthetic preprocess lines no longer leak into diagnostics** (v0.8.0) — `SanitisedDiagnostic` wraps every emitted diagnostic.
- **Diagnostic hints polished** (v0.8.0): multi-line `|>` chains, `freeze let` at non-module scope, `wrong_arg_count` kw-only rephrasing, collection variance suggesting `Sequence[Animal]` / `Mapping[K, V]` / `frozenset[T]`, dict-to-model mismatch pointing at the constructor form.
- **New lint warnings** (v0.8.0): `tyc::empty_collection_no_annotation`, `tyc::typing_alias_in_annotation`, `tyc::contains_secret_literal`.
- **CLI polish** (v0.8.0): `tyc check lib.dty` accepts a single `.dty` file directly; `tyc run --compile` rejects single-file inputs up-front; `tyc migrate` strips trivial `__init__` methods.
- **Default change** (v0.8.0): `unused_import` is now `warn` (was `error`). Restore via `[strictness] unused-import = "error"`.

### v0.7.1 — LSP bugfix

- **LSP semantic-tokens positions** now align with the original `.ty` source instead of the preprocessed Python view. Pure bugfix; no language or runtime changes.

### Earlier highlights since v0.3.0

### Type-system relaxations

- **`bool ⊆ int`** (v0.4.0) — `let x: int = True`, `1 + True`, `-True` all check.
- **De Morgan narrowing** (v0.4.0) — `if not (A or B): return` narrows both operands afterwards.
- **Fixed-arity tuple covariance** (v0.4.0) — `tuple[int, int]` widens both slots to `float`.
- **Subclass constructors accept inherited fields** (v0.4.0).
- **Dotted-attribute annotations** resolve to foreign class shapes (v0.4.0).
- **Variance table** expanded with `AsyncContextManager`, `KeysView`, `ValuesView`, `ItemsView`, `Type`, `Counter` (v0.5.0).
- **`set - set` / `frozenset - frozenset`** type-check (v0.5.2).
- **`Ok` / `Err` Result combinators** as methods (v0.6.0): `.map`, `.map_err`, `.and_then`, `.or_else`.
- **`impl` on a sealed-union alias** distributes to every variant (v0.6.0).
- **Cross-module variant → sealed-union flow** (v0.6.0).
- **Cross-module function signatures preserve param/return types** (v0.6.0).
- **`pub freeze let X = …`** parses (v0.6.0).
- **`pub def` visible to `?` validator** (v0.6.0).
- **For-target doesn't rebind prior `let` bindings** (v0.6.0).
- **Sibling `case` arms: `let` declarations don't shadow each other** (v0.6.0).
- **`pub *` wildcard re-export aggregation** in `__init__.ty` (v0.7.0).
- **Declare-only `let NAME: T`** with definite-assignment analysis (v0.7.0).
- **`with cm() as r:` / `async with cm() as r:`** types `r` from `__enter__` / `@contextmanager`-decorated factories (v0.7.0).
- **`await Callable[..., Awaitable[T]]`** unwraps to `T` (v0.7.0) — canonical async-middleware `let r: Resp = await next(req)` works.
- **Same-newtype arithmetic preserves the newtype** (v0.7.0).
- **Ternary narrowing** (v0.7.0) — `body if test else orelse` narrows like `if`/`else`.
- **Cross-module generic method dispatch** propagates class TypeVars (v0.7.0).
- **`from X import Y`** inside `if`/`for`/`while`/`with`/`try`/`match` arms binds (v0.7.0).
- **Sibling `if`/`elif` branches** don't trip `no_block_shadow` (v0.7.0).
- **Multi-line `go expr(...)`** parses (v0.7.0).

### Diagnostics added

- `tyc::duplicate_method` (v0.3.1).
- `tyc::stdlib_module_shadow` (v0.6.0, refined v0.7.0) — `.ty` file shadowing Python 3.13 stdlib top-level module names.
- `tyc::pub_name_collision` (v0.7.0).
- `tyc::pub_star_outside_init` (v0.7.0, advice).
- `tyc::use_of_uninitialised` (v0.7.0).
- `tyc::field_default_ordering` (v0.7.0).

### CLI additions

- **`tyc run` gates the VM behind a static `tyc check`** (v0.3.1). Set `TYC_SKIP_CHECK=1` to bypass; `--compile` always gates on the full build.
- **`tyc migrate` rewrites `Generic[T]` → PEP 695** (v0.3.1).
- **`tyc migrate @dataclass(frozen=True)` → `class X frozen:`** (v0.5.0).
- **`tyc migrate class X(Protocol[T]):` → `interface X[T]:`** (v0.5.0).
- **`tyc migrate NAME = NewType("NAME", BASE)` → `newtype NAME = BASE`** (v0.5.0).
- **`tyc ty` diagnostic attribution** via `.py.map` (v0.5.0) — rewrites `path.py:LINE:COL` to `.ty` coordinates. `--raw` opts out.
- **`tyc debug` Typhon-aware pdb wrapper** (v0.5.0) — surfaces `[ty] <src>:<line>` after every pause; loads all `.py.map` at startup. `--raw-pdb` opts out.
- **`tyc debug --break ty-file:line`** translates `.ty` coordinates through `.py.map` (v0.5.0).
- **Grouped `tyc check` diagnostics by source file** (v0.3.1) plus per-code summary tally.
- **`tyc explain --list`** prints every diagnostic code (v0.6.0 docs).
- **`.py.map` sidecars move to `<out>/.sourcemaps/`** (v0.6.1); resolvers prefer the new location, legacy adjacent layout still readable.

### "Designed but NOT yet supported" — avoid these forms

- **`lazy let X: T:` colon-block form** does NOT parse. Use `lazy let X: T = expr`.
- **`pub lazy let` does NOT parse** (v0.15.4). `pub` stacks with every other modifier — including `comptime` (`pub comptime let` works) — but the `lazy let` lowering runs in a separate text pass that would silently drop the laziness under a leading `pub `, so it is intentionally rejected rather than half-working. Declare a module-level `lazy let` without `pub` (it's importable by name regardless), or use `pub comptime let` when a build-time constant fits.
- **`lazy[T]` return-type form** is designed but unimplemented.
- **Multi-line `|>` pipes require wrapping parens.**
- **`model X frozen:` does NOT parse** — `frozen` is on `class` only.
- **`let`-shadowing is rejected** — use `mut` or a fresh name; never re-bind with `let`.
- **`from typing import TypeVar`** is rejected — use PEP 695.
- **`from typing import List` / `Dict` / `Tuple` etc.** is rejected — use lowercase builtins.
- **Bounded TypeVars** parse; multi-argument constraint solving is partial.
- **PEP 612 `ParamSpec`** is not modelled — annotate decorator layers with `Callable[..., Any]`.

---

## 8. `let`, `mut`, and what immutability means

`let`/`mut` govern **binding immutability**, not deep value immutability — same as Rust's `let`/`let mut` or TypeScript's `const`/`let`. `let u: User` cannot be reassigned, but `u.name = "x"` is still legal if `User` has a mutable `name` field.

For deep immutability on instances, use `class P frozen:` (emits `frozen=True` on the underlying dataclass / Pydantic config). Note: dataclass `frozen=True` only blocks field reassignment — nested mutable containers can still be mutated. Use `tuple` / `frozenset` inside frozen classes for stronger guarantees.

For deep immutability on module-level bindings, use `freeze let CONFIG = {...}` (v0.3.0) — the value is recursively frozen via `typhon_runtime.freeze.deep_freeze` at startup.

Parallelisation passes refuse to touch any binding captured as `mut` by a spawned task without explicit sync.

Top-level module bindings default to `let` unless declared `mut`. Inside functions, the keyword is always explicit.

### Typed tuple unpacking (v0.3.1)

```python
let (a: int, b: str) = func(x, y)
let (a: int, b)      = pair()       # mixed; un-annotated leg uses inference
let (xs: list[int], ys: list[int]) = split()  # compound annotations OK
```

Desugars to a hidden `__typhon_unpack_N__` temp plus per-element typed assigns. The top-level-comma split inside the annotation pair survives compound annotations like `list[int]`, `dict[str, int]`, `tuple[float, ...]`.

### Declare-then-assign `let NAME: T` (v0.7.0)

```python
def parse(raw: str) -> Result[Cfg, str]:
    let loaded: Cfg
    match _load(raw):
        case Ok(v):  loaded = v
        case Err(e): return Err(e)
    return Ok(loaded)
```

The resolver tracks each uninitialised `let` declaration's span; the FIRST subsequent assignment silently succeeds (it IS the initialiser). The standard `tyc::immutable_assign` fires on any SECOND assignment. Sibling `match` arms and sibling `if` / `elif` / `else` bodies each count as a separate first-assignment path. `mut NAME: T` without initialiser is also accepted (any number of subsequent assignments legal). Reads on a path that hasn't assigned fire `tyc::use_of_uninitialised` with labels on both use site and declaration.

---

## 9. Error handling with `Result[T, E]`

`Result[T, E]` is a sealed sum with `Ok(value: T)` and `Err(error: E)`. Emits as frozen dataclasses in a generated `typhon_runtime/result.py` — no PyPI dep.

```python
def parse_port(raw: str) -> Result[int, str]:
    if not raw.isdigit():
        return Err(f"not a number: {raw}")
    let n: int = int(raw)
    if n < 1 or n > 65535:
        return Err(f"out of range: {n}")
    return Ok(n)
```

### `?` propagation

```python
def parse_addr(host: str, raw: str) -> Result[tuple[str, int], str]:
    let port: int = parse_port(raw)?     # unwrap Ok, short-circuit Err
    return Ok((host, port))
```

`?` is **not** `try/except`. It desugars to:

```python
_tmp_0 = parse_port(raw)
if isinstance(_tmp_0, Err):
    return _tmp_0
port: int = _tmp_0.value
```

Stack traces stay clean. The checker enforces:

- `?` only appears inside a function whose return type is a compatible `Result`. `?` in a non-`Result` fn → `tyc::invalid_question_op`.
- Error types must match (or unify under generics). Mismatches → `tyc::result_error_mismatch`. Convert at the boundary via `match`.
- `?` inside a comprehension is rejected (cannot hoist out of the comp's scope; v0.3.1).
- Inline `?` is supported (v0.3.0): `Ok(add(parse(s)?, parse(t)?))` works.

### `with`-chains

For 3+ chained Results:

```python
def make_report(uid: int) -> Result[Report, AppError]:
    with user   = db.find(uid)?,
         perms  = check(user)?,
         report = build(user, perms)?:
        return Ok(report)
    else err:
        log.warn(err)
        return Err(err)
```

The `else err:` block is optional — without it, the first `Err` short-circuits via the enclosing function (which must return a compatible `Result`).

### Combinators (v0.6.0)

`Ok` and `Err` carry `.map`, `.map_err`, `.and_then`, `.or_else` methods. For heterogeneous error pipelines:

```python
let toks: Tokens   = tokenize(src).map_err(_lex_to_pipeline)?
let ast:  Ast      = parse(toks).map_err(_parse_to_pipeline)?
let ty:   TypedAst = check(ast).map_err(_type_to_pipeline)?
```

Semantics: `Ok.map(f)` transforms value, `Ok.map_err(g)` is identity (vice versa for `Err`); `and_then` chains a `Result`-returning op on `Ok`; `or_else` recovers from `Err`.

### Bridging exceptions

Wrap library boundaries in a small `try` shim:

```python
import json

def load(path: str) -> Result[dict[str, str], str]:
    try:
        with open(path) as f:
            return Ok(json.load(f))
    except FileNotFoundError:
        return Err(f"not found: {path}")
    except json.JSONDecodeError as e:
        return Err(f"invalid JSON: {e}")
```

After the shim, downstream code uses `?` and `with`-chains without ever writing `try`.

For a **single** boundary call, the `try_result` combinator (v0.15.0) collapses the shim into one expression — a prelude name (no import) typed as `Result[T, E]`:

```python
def load(path: str) -> Result[dict[str, str], str]:
    return try_result(lambda: read_json(path), lambda e: f"invalid JSON: {e}")
```

`try_result(thunk)` runs `thunk()` and returns `Ok(result)`; on any exception it returns `Err(on_err(exc))`, or `Err(exc)` (the raw exception, `Result[T, Exception]`) when the mapper is omitted. `T` is inferred from the thunk body, `E` from the mapper body. It works under `tyc run` (the VM materialises the caught exception just as an `except E as e:` handler would) and the compiled path (`from typhon_runtime import try_result` is auto-injected). Use the explicit multi-`except` `try` shim when you map *distinct* exception types to *distinct* errors; reach for `try_result` for the common single-boundary case.

### `rescue` — lambda-free exception boundaries (v1.0.0-alpha)

`rescue` is the no-lambda, no-`try`/`except` surface for the same bridge. Two forms:

```python
# postfix — catch EXPR, map the exception, propagate the Err like `?`
def parse_port(raw: str) -> Result[int, ConfigError]:
    let n: int = int(raw) rescue e: BadField(field="port", reason=str(e))
    return Ok(n)

# block — map any exception raised across a whole suite (replaces a try/except shim)
def load_config(text: str) -> Result[Config, ConfigError]:
    rescue e: BadJson(reason=str(e)):
        let data: dict[str, str] = json.loads(text) as! dict[str, str]
        let port: int = parse_port(data["port"])?
        return Ok(Config(host=data["host"], port=port))
```

- **Postfix `EXPR rescue NAME: ERR`** lowers (in `tyc-syntax`'s `expand_rescue`, folded into `expand_question_ops`) to `try_result(lambda: EXPR, lambda NAME: ERR)?`. The left operand is found with the same bracket-/string-/comment-aware scan `as!` uses, so it works in value positions and after a leading `return`/`if`/`while`/`assert`, and composes with `as!` (`json.loads(t) as! dict[str, str] rescue e: …`). **Scope:** lowered in **statement-tail** position (the last thing on the logical line — the `…)?` shape every pipeline's end-of-line `?` pass handles); an inline/mid-expression postfix `rescue`, or one whose right side isn't `NAME: EXPR`, is left for the parser.
- **Block `rescue NAME: ERR:`** over a suite lowers (in `expand_rescue_blocks`, run ahead of the postfix pass, to a fixpoint so nested blocks expand) to `try: <suite> except Exception as NAME: return Err(ERR)`. It emits a real `Err(...)`, so the checker's `return Err(...)` error-type check validates `ERR` against the function's declared error type.
- The mapped error type **is** checked against the enclosing function's error type in both forms (`tyc::result_error_mismatch`) — the v1.0.0-alpha work also fixed f-strings inferring as `Unknown` (they now infer as `str`), which had let an f-string mapper slip a `Result[T, Unknown]` past `?`.
- Both forms run identically under `tyc check`, `tyc run` (VM), and `tyc build` + CPython; `tyc fmt` round-trips them. Worked example: `examples/60-rescue-boundaries/`.

---

## 10. Async and concurrency

### Explicit `async`, not inferred

- A sync function calling an `async` one without `await` is a **hard error** (`tyc::missing_await`).
- An `async` function with no `await` is a **warning** (`tyc::async_without_await`).
- Direct call to known-blocking stdlib (`time.sleep`, `requests.*`, `subprocess.run`, …) inside `async def` → **`tyc::blocking_in_async`** (warn).
- `loop.run_until_complete(coro())` does NOT fire `tyc::missing_await` (v0.6.0).

### `gather:` — parallel awaits

```python
async def load(uid: int) -> Dashboard:
    gather:
        user   = fetch_user(uid)
        posts  = fetch_posts(uid)
        notifs = fetch_notifs(uid)
    return Dashboard(user=user, posts=posts, notifs=notifs)
```

Lowers to `asyncio.TaskGroup` (cancel-on-failure):

```python
async with asyncio.TaskGroup() as _tg:
    _t_user   = _tg.create_task(fetch_user(uid))
    _t_posts  = _tg.create_task(fetch_posts(uid))
    _t_notifs = _tg.create_task(fetch_notifs(uid))
user   = _t_user.result()
posts  = _t_posts.result()
notifs = _t_notifs.result()
```

Bindings inside the `gather:` block are an intentional exception to Rule 2 — they don't need `let`/`mut` because the keyword itself introduces them as immutable single-assignment names. Dependent bindings (one references an earlier binding) gracefully degrade to sequential `await` in source order.

For best-effort semantics where each binding becomes `T | Exception`:

```python
gather(strategy="best-effort"):
    user = fetch_user(uid)
    posts = fetch_posts(uid)
```

Lowers to `asyncio.gather(..., return_exceptions=True)`.

### Automatic `gather` (opt-in)

`[strictness] auto-gather = true` rewrites straight-line runs of independent `name = await callee(...)` into a `TaskGroup`, **but only when every callee is a same-module `async def` carrying `@gatherable`** and the LHS bindings don't alias. Imported async callees are left untouched so flipping the flag doesn't surprise upstream callers.

```python
@gatherable
async def fetch_user(uid: int) -> User: ...

@gatherable
async def fetch_posts(uid: int) -> list[Post]: ...

async def load(uid: int) -> Dashboard:
    let user  = await fetch_user(uid)     # rewritten into a TaskGroup …
    let posts = await fetch_posts(uid)    # … because both callees are @gatherable
    return Dashboard(user=user, posts=posts)
```

When a run of 2+ adjacent independent awaits would have been gathered but at least one callee lacks `@gatherable`, `tyc build` surfaces a `tyc::auto_gather_missed` advice-level diagnostic naming the missing callee. (Only fires with `[strictness] auto-gather = true`; the nudge is silent otherwise.)

Cross-module reach (v0.14.2): `auto-gather` now folds independent awaits whose callees are `@gatherable` async functions **imported from another project module**, not just same-module ones. The `@gatherable` attestation is still required, so an imported callee without it is never folded.

### `tyc::gather_opportunity` — concurrency advice, on by default (v0.14.2)

Independent of `auto-gather`, `tyc check` / `tyc build` flag every run of 2+ adjacent independent awaited calls inside an `async def` and suggest wrapping them in an explicit `gather:` block:

```python
async def load(client: Client, uid: int) -> tuple[User, list[Post]]:
    let user = await client.get_user(uid)     # advice: these 2 awaits
    let posts = await client.get_posts(uid)   # could run concurrently
    return (user, posts)
```

Unlike `auto_gather_missed`, it is **callee-agnostic** — it fires for awaited **method calls on imported clients** (the most common missed-concurrency shape, which `auto-gather` never touches) because the suggested fix (an explicit `gather:`) works for any awaitable with no `@gatherable` decorator. Independence is decided by static data flow: a run breaks the moment a later await references a name bound earlier — including through the callee's receiver (`b = await a.next()`), keyword args, comprehensions, walrus, slices, f-strings. It **never rewrites** (concurrency is a behaviour change the author opts into) and is **advice-level** (the `☞` badge; never blocks a build, surfaces as an editor hint via the LSP). Knob: `[strictness] suggest-gather` (default `true`; set `false` to silence). When `auto-gather` is also on, runs it folds are gone before this pass runs, so they aren't double-reported. `tyc explain gather_opportunity` documents it offline.

### `go` — fire-and-forget

```python
async def signup(email: str) -> User:
    let user: User = await create(email)
    go send_welcome(user)            # registered with strong ref
    return user
```

Or capture the handle:

```python
go send_welcome(user) -> task
await task                          # later
```

`go` lowers through `typhon_runtime.tasks.spawn`, **never** to a bare `asyncio.create_task` — Python's event loop holds only weak refs, so fire-and-forget can be GC'd mid-flight. The runtime registry holds strong refs and clears entries from a done-callback.

Multi-line `go expr(...)` parses (v0.7.0).

### Async-callable awaits (v0.7.0)

```python
async def middleware(next: Callable[[Req], Awaitable[Resp]], req: Req) -> Resp:
    let resp: Resp = await next(req)        # ✅ unwraps Awaitable[Resp] to Resp
    return resp
```

`await` on a `Callable[..., Awaitable[T]]` / `Coroutine[Y, S, T]` call unwraps to `T`. This unblocks canonical async-middleware shapes.

### Free-threaded mode

`[python] free-threaded = true` (requires 3.13t / 3.14t / 3.15t):

- `go` on CPU-bound functions lowers to `ThreadPoolExecutor.submit`.
- The analyser may parallelise pure-function comprehensions via `typhon_runtime.parallel.map_pure(...)`, gated by `[strictness] auto-parallel` and `[strictness] parallel-min-size` (default 64). Set, dict, and list comprehensions are all eligible (v0.5.0), including a pure `if` filter, extra literal/`let`-bound-invariant call arguments, and nested pure calls (`g(f(x))`).
- `[strictness] auto-parallel-reductions` (requires `auto-parallel`) additionally parallelises `for x in xs: total += EXPR` accumulator loops with a **plain `int`** accumulator (`mut total: int`) and a pure `EXPR` — integer addition is exact/associative so partial sums combine identically in any order; `float` accumulators are never rewritten.
- `[strictness] parallel-backend` (default `"threads"`) selects the executor `map_pure` uses; `"interpreters"` tries a PEP 734 `InterpreterPoolExecutor` (3.14+) first, falling back transparently to the thread pool on an older runtime or an unshareable mapped callable.
- Two advice lints, both gated by `[strictness] suggest-parallel` (default `true`) and both silent unless `free-threaded = true`: `tyc::parallel_opportunity` nudges a parallelisable comprehension/reduction whose enabling knob is off (or a `float` accumulator that's ineligible only for its type); `tyc::shared_mut_across_tasks` flags a `go`-spawned same-module function that writes a `global` or module-level `mut` binding — a data race under real concurrency.
- Every parallel block runtime-checks `sys._is_gil_enabled()` and falls back to sequential if a GIL build is detected.

Default off until 3.14 is the default Python.

---

## 11. Lazy loading

```python
lazy import np = numpy           # ✅ deferred via bespoke `__TyphonLazy_np_` proxy class
lazy from numpy import array     # ❌ rejected at parse time (PEP 690 reasoning)
```

`lazy from ... import` defeats deferral (it eagerly touches attributes on the source module) and is a hard parse error. Redirect to `lazy import` + dotted access.

On a **3.15+ target**, `lazy import ALIAS = MODULE` lowers to the native [PEP 810](https://peps.python.org/pep-0810/) `lazy import MODULE as ALIAS` statement instead of the `__TyphonLazy_*` proxy class — no `typhon_runtime` dependency, and a project whose only runtime-touching feature was `lazy import` ships no generated `typhon_runtime/` package at all on that target. 3.13 / 3.14 output is unchanged.

Other lazy forms:

| Form | Lowers to |
|---|---|
| `lazy let CFG: Config = load()` (module-level) | sentinel-cached `lazy_let(lambda: load())` in `typhon_runtime` (thread-safe, one-shot) |
| `lazy let cfg: Config = load()` inside class body | `@cached_property` (per-instance is the intended scope) |
| `def primes(n: int) -> lazy[list[int]]:` | designed but **unimplemented today** — use `Iterator[int]` directly |

Module-level lazy bindings use the runtime helper rather than `functools.cached_property` because the latter is instance-scoped, race-prone, and writable after first eval.

**Doc-confirmed non-working forms** (avoid these):
- `lazy let X: T:` colon-block form does NOT parse.
- `lazy[T]` return type form is designed but unimplemented.

---

## 12. Compile-time evaluation (`comptime`)

`comptime` bindings are evaluated **at build time** in a sandboxed interpreter; results are inlined as literals.

```python
comptime let PORT: int = int(env("PORT", "8080"))
comptime let DB_URL: str = env("DATABASE_URL")   # build fails if unset
comptime let IS_PROD: bool = env("BUILD_TAG", "dev") == "prod"
comptime let HOST: str = env("HOST", "localhost").lower()

comptime def feature(name: str) -> bool:
    return env("FEATURE_" + name.upper(), "0") == "1"

comptime let SHIPS_AUTH: bool = feature("auth")

comptime let T: type = int        # v0.5.0 — types-as-comptime-values
```

Declare required env vars in `typhon.toml`:

```toml
[env]
required = ["DATABASE_URL"]
```

### Allowed in the sandbox

- **Statements (in `comptime def` body):** `return`, local bindings (`x = …`, `let x: T = …`, `mut x: T = …`), `if`/`elif`/`else`.
- **Expressions:** literals (`int`, `float`, `str`, `bool`), container literals (`[1, 2]`, `{"a": 1}`, `(1, "x")`, including empty containers and the trailing-comma single-element tuple form), arithmetic (`+ - * / // % **`), comparisons (`== != < <= > >=`), boolean ops (`and or not`), ternaries (`x if cond else y`), `env(name, default?)`, the `int()` / `str()` / `float()` / `len()` casts, a small pure-only set of string methods (`upper`, `lower`, `strip`, `lstrip`, `rstrip`, `replace`, `startswith`, `endswith`, `split`, `join` (v0.3.1)), subscript with Python negative indexing, calls to user-defined `comptime def` functions.
- **Types-as-values (v0.5.0):** `int`, `str`, `bool`, `float`, `bytes`, `None`, `type`, `object`. Stored as `ComptimeValue::Type(...)`.

### Forbidden

- Loops, exceptions (`raise`, `try`/`except`), `with`-blocks, `class` declarations, nested `def` (other than the outer `comptime def`), arbitrary imports.
- I/O, network, subprocess, random, time, uuid, `os.urandom`.
- Free variables (module-level names that aren't parameters or local bindings) — comptime is **hermetic**; pass everything in as arguments.
- `*args`, `**kwargs`, defaults, keyword-only parameters on a `comptime def`.
- Recursion depth capped at **64**.

### Emitted Python

```python
PORT: int = 8080
DB_URL: str = "postgresql://..."
SHIPS_AUTH: bool = True
```

`comptime def` functions are **also preserved** in the emitted `.py` so they remain callable at runtime.

### Secret-shape literal warning

`tyc::contains_secret_literal` (warn) fires when a `comptime let` binding's name matches `*KEY`, `*TOKEN`, `*PASSWORD`, `*SECRET`, `*PASS`, `*PWD` — the build artifact would contain the resolved env-var value as a string literal. Read at runtime via `os.environ[...]` instead.

---

## 13. `typhon.toml` reference

Default scaffold (`tyc init`):

```toml
[project]
name = "myapp"
version = "0.1.0"
src = "src"
out = "build"

[python]
target = "3.13"                  # **required: 3.13+ only**. Valid: "3.13" / "3.13t" / "3.14" / "3.14t" / "3.15" / "3.15t". Older values are rejected at config load. 3.15+ unlocks native PEP 810 lazy-import lowering (see §11).
free-threaded = false            # requires 3.13t/3.14t/3.15t; off by default

[optimise]
level = 0                        # 0 (default) | 1 — flips auto-memoise/auto-gather/auto-parallel/pgo-memoise defaults to true. Explicit [strictness] entries always win. `tyc build -O` applies level 1 for one invocation.

[emit]
class-default = "dataclass"      # only "dataclass" today; project-wide "pydantic" is rejected (use `model` per class). Unknown values → tyc::invalid_config_value
format = true                    # post-process through ruff format
model-extra = "forbid"           # "forbid" | "allow" | "ignore"
skip-decoration-bases = []       # extra base-class names suppressing the auto @dataclass decoration. Matched by last segment.
traceback-remap = false          # (v0.14.0) inject a `.ty`-source traceback remapper into the entry `__main__`
# pyi-stubs is always on — every .dty emits a .pyi

[strictness]
no-implicit-any = true           # reserved for forward compat; today the check is always on
unused-import = "warn"           # default "warn" (since v0.8.0); or "error" | "off"
exhaustive-match = "error"
methods-in-class-body = "warn"   # or "error" (break CI) | "off"
nullable-use = "warn"            # severity of the attribute-rooted tyc::nullable_use form (`self.conn.execute()`); "error" promotes it, "off" drops it. The bare-name form is always an error.
require-with = "warn"            # severity for tyc::resource_not_managed
blocking-in-async = "warn"       # severity for tyc::blocking_in_async
stub-check = "error"             # severity for tyc::stub_mismatch
suggest-gather = true            # advice: surfaces tyc::gather_opportunity
auto-memoise = false             # opt-in; caches *provably* pure fns with immutable, hashable params + return (defaults true at [optimise] level = 1)
auto-gather = false              # opt-in; folds independent awaits into TaskGroup (needs @gatherable; defaults true at level = 1)
auto-parallel = false            # opt-in; pure list/set/dict comprehensions → thread-pool map (defaults true at level = 1)
auto-parallel-reductions = false # opt-in; parallelises `mut total: int` accumulator loops (requires auto-parallel; int-only — exact/associative, floats never reordered)
parallel-min-size = 64
parallel-backend = "threads"     # or "interpreters" — PEP 734 InterpreterPoolExecutor (3.14+) with transparent fallback to threads
pgo-memoise = false              # opt-in; promotes hot pure fns from typhon-profile.json (defaults true at level = 1)
pgo-min-calls = 100
suggest-perf = true              # advice: surfaces the tyc::perf_* micro-optimisation lint family
suggest-parallel = true          # advice: surfaces tyc::parallel_opportunity / tyc::shared_mut_across_tasks (free-threaded only)
unintrospectable-dependency = "warn"  # (v0.12.0) "warn" | "error" | "off" — declared dep imported but not introspectable
allow-secret-comptime = false    # set true to silence tyc::contains_secret_literal

[checker]                        # (v0.12.0) second-stage checker over the EMITTED Python
external = "none"                # or "ty" — runs Astral's `ty` against typeshed; errors fail the build
external-args = []               # extra flags forwarded verbatim to the external checker

[env]
required = ["DATABASE_URL"]      # comptime env() lookups that must resolve at build

[dependencies]
requests = ">=2.31"
rich = "*"                       # bare name → any version

[dev-dependencies]
pytest = "8.2"                   # bare version → ==8.2
```

Notes on always-on behaviour:

- **PEP 561 `.pyi` stubs are always emitted** alongside every `.dty`.
- **Pydantic emissions inject `model_config = ConfigDict(extra=…)`**, controlled by `model-extra`.
- **`tyc::stdlib_module_shadow`** is gated on the presence of `typhon.toml` (standalone-file checks skip it).

`[optimise] level` (default `0`) is a single project-wide dial: `level = 1` flips the *default* of `auto-memoise` / `auto-gather` / `auto-parallel` / `pgo-memoise` to `true`, but an explicit `[strictness]` entry for any of those four always wins over the level-derived default — so a project can opt into level 1 and still keep one knob off by setting it explicitly. `tyc build -O` / `--optimise` (alias `--optimize`) applies `level = 1` for a single invocation without editing `typhon.toml`; it overrides a config `level = 0` but never an explicit `[strictness]` setting.

`[checker] external = "ty"` (v0.12.0) is the only path that type-checks against **typeshed**, so it covers C-extension and stdlib APIs that runtime venv introspection can't model. It spawns `ty check` over the build output and re-attributes diagnostics to `.ty` via `.py.map`. Requires `ty` on `PATH` (`pip install ty` / `uv tool install ty`); most useful with the project's deps installed in a venv. `--with-ty` on `tyc build` / `tyc check` runs the same pass for one invocation without editing `typhon.toml`.

`unintrospectable-dependency` (v0.12.0) covers the most dangerous failure mode of third-party checking: a *skipped* check looked identical to a clean pass. It fires when a declared, imported dependency can't be introspected (no reachable `.venv` / `python3`, not installed, or no introspectable signatures). Clear it by installing deps (`uv sync`) or shipping a `.dty` stub. Per-top-level success tracking means a package whose root introspects fine isn't flagged because one submodule failed.

`allow-secret-comptime` (lives in `[strictness]`; wired through in v0.10.0, seeded into `tyc init`'s scaffold in v0.11.0) silences `tyc::contains_secret_literal` when set `true`.

`traceback-remap` (v0.14.0, `[emit]`, default `false`) injects `typhon_runtime.traceback.install()` at the top of the entry module's `if __name__ == "__main__":` block. The installed `sys.excepthook` loads the emitted `.py.map` sidecars and rewrites each `File "…​.py", line N` traceback frame to the corresponding `.ty` location — the same mapping `tyc trace` applies, but automatically and only for the entry script (library imports never trip the `__main__` guard). It falls back to the previous hook on any failure, so it can only improve a traceback, never suppress one. Default-off keeps existing projects and runtime-free entry points byte-for-byte unchanged (a project with no other runtime feature stays free of the generated `typhon_runtime/` package).

`auto-parallel-reductions` (requires `auto-parallel`) additionally parallelises integer accumulator loops — `for x in xs: total += EXPR` with `mut total: int` and a pure `EXPR` lowers to `total += sum(map_pure(lambda x: EXPR, xs))`. Integer addition is exact and associative/commutative, so partial sums combine correctly in any order; `float` accumulators are never rewritten (reordering IEEE-754 addition changes the result). `parallel-backend = "interpreters"` makes the generated `typhon_runtime/parallel.py` try a PEP 734 `InterpreterPoolExecutor` (3.14+) first, falling back transparently to the thread pool on an older runtime or an unshareable mapped callable — order is preserved on every path. `suggest-perf` (default `true`) gates the `tyc::perf_*` micro-optimisation family (six lints) plus `tyc::lazy_import_opportunity` — seven advice lints in total; `suggest-parallel` (default `true`) gates `tyc::parallel_opportunity` and `tyc::shared_mut_across_tasks`, both of which additionally require `[python] free-threaded = true` to fire. All are advice-only — never block a build. See [DIAGNOSTICS.md](DIAGNOSTICS.md).

`auto-gather` independence rules:

- Every callee must be a same-module `async def` carrying `@gatherable`.
- LHS bindings must not alias.
- The statements must form a straight-line block.

---

## 14. `tyc` subcommand reference

See [CLI.md](CLI.md) and `docs/cli.md` for the full surface. The most-used commands:

| Command | What it runs | When |
|---|---|---|
| `tyc check src/` | parse → resolve → type → analyse (no emit) | CI; daily editing |
| `tyc build` | full pipeline through emit + ruff format; `--check` for dry-run; `-O`/`--optimise` applies `[optimise] level = 1` for one invocation | local run; produces `build/*.py` + `build/.sourcemaps/*.py.map` |
| `tyc fmt src/` | in-process whitespace pass + `ruff format` wrap (when on PATH) | pre-commit |
| `tyc run` | execute a Typhon program in the in-process VM by default; `--compile` (alias `--no-vm`) falls back to build-then-exec for CPython library interop | iterating on pure-Typhon code |
| `tyc lsp` | LSP on stdio (diagnostics, hover, go-to-def, member completions via venv introspection, from-import members from sibling files, "Remove unused import"; v0.12.0 also surfaces live third-party arg/type diagnostics via `tyc-venv`) | editor |
| `tyc init NAME` | scaffold `typhon.toml`, `src/`, `tests/` with a worked `main.ty` (frozen dataclass + `impl` + `Result`/`?`/`match`) | new project |
| `tyc trace traceback.txt` | map Python frames back to `.ty` via `.py.map` v2 | debugging emitted code |
| `tyc profile` | build, then instrument every top-level fn with call-count + wall-clock (it does not run the program — you run the instrumented build from the project root, and `typhon-profile.json` is written on interpreter exit; the entry module's functions record as `__main__.<fn>`, accepted alongside `main.<fn>` for `src/main.ty`) | feeds `pgo-memoise` |
| `tyc migrate src/app.py` | typed Python → Typhon: rewrites `Optional[T]`/`T \| None` → `T?`, `Generic[T]` → PEP 695, `NewType` → `newtype`, `Protocol` → `interface`, drops `@dataclass`/`@dataclass(frozen=True)`, adds `let`/`mut` to module-level annotated assigns | `--check` for CI |
| `tyc ty` | builds, then runs Astral's `ty` checker over emitted Python with `.ty` path attribution via `.py.map` (v0.5.0). The same pass runs automatically when `[checker] external = "ty"` or `--with-ty` is set on build/check (v0.12.0) | second-opinion type-checking; needs `pip install ty` |
| `tyc stubtest` | builds, then runs `python -m mypy.stubtest` against every emitted `.pyi` | runtime probe complementing `tyc check --stubs` |
| `tyc repl` | interactive evaluator; compiles each block through the full pipeline | quick experiments; `:quit` / `:reset` / `:show` |
| `tyc debug` | builds + execs `python -m pdb build/main.py` with a source-mapping wrapper (v0.5.0) that surfaces `[ty]` paths; `--break <ty-file>:<line>` translates `.ty` coordinates through `.py.map` | step through emitted code; pair with `tyc trace` |
| `tyc explain <code>` | prints the catalog entry for a `tyc::` diagnostic (short or fully-qualified code); `tyc explain --list` prints every code | when a diagnostic needs more context |
| `tyc cheatsheet` | prints the 30-second Typhon cheat sheet to stdout | offline syntax refresher |
| `tyc add` / `tyc remove` / `tyc sync` | manage `[dependencies]` / `[dev-dependencies]`, shell to `uv` | package management |
| `tyc install skill` | write the embedded `typhon` Claude skill (this `SKILL.md`, every sibling reference, and the `references/` examples) into `.claude/skills/typhon/` of the current project; `--force` overwrites, `--dir PATH` targets another root, `--list` previews the files | vendoring the skill into another repo |

Notable flags:

- `tyc check --stubs` — also diff every `.dty` against the runtime module it describes.
- `tyc check --with-ty` / `tyc build --with-ty` (v0.12.0) — run the `ty` typeshed pass for this one invocation without editing `typhon.toml`. `tyc check --with-ty` (normally emit-free) builds to a throwaway directory first.
- `tyc build --check` — dry-run, lists every file that would be written without touching disk.
- `tyc build --no-sync` (or `TYC_NO_SYNC=1`) — skip `uv sync` but still merge `pyproject.toml`.
- `tyc build -O` / `--optimise` (alias `--optimize`) — apply `[optimise] level = 1` for this invocation; an explicit `[strictness]` entry still wins.
- `tyc run --compile` (alias `--no-vm`) — fall back to build-then-exec when the program imports CPython-only libraries.
- `tyc run --temp` — compile mode only; build into a tempdir deleted on exit.
- `tyc ty --watch` / `tyc ty --out DIR` / `tyc ty -- --strict` / `tyc ty --raw`
- `tyc repl --load src/lib.ty` / `tyc repl --python python3.13`
- `tyc debug --entry api.py --debugger pudb --break src/main.ty:42` / `tyc debug --raw-pdb`
- `tyc lsp --log-level debug` — threshold (`error` / `warn` / `info` / `debug`, default `info`) for the status messages forwarded to the editor over `window/logMessage`.
- `tyc trace err.log --map-dir DIR` — extra directory to search for `.py.map` sidecars; with no file argument, `tyc trace` reads the traceback from stdin.
- `tyc fmt --check src/` (`-c`) — exit non-zero if anything would change; `fmt`'s only flag.
- `tyc init NAME --dir DIR` (`-d`) — scaffold into `<DIR>/<NAME>/`; `tyc add` / `tyc remove --dir DIR` edit another project's `typhon.toml`.
- `tyc add --dev pytest@8.2` / `tyc add --no-sync` / `tyc sync --dry-run`

`tyc repl` quirks: each prompt re-executes the entire accumulated session (pure-scratch semantics, side effects fire once per prompt), multi-line blocks end on the first blank line, no readline/arrow-key support yet. Bare single-line expressions auto-print their `repr(...)` — `>>> 1 + 1` prints `2`.

`tyc debug` v0.5.0 wraps `pdb.Pdb` so the **entire debugger UI reads `.ty` coordinates** — `list`, `where`, stack frames, and the prompt all display `.ty` paths and source slices. `--raw-pdb` opts out. `--break <ty-file>:<line>` translates Typhon source locations through `.py.map` and forwards them to the chosen debugger as `-c "break …"`.

`tyc run` defaults to the in-process `tyc-vm` tree-walking interpreter — no `.py` written, no CPython spawn. As of v0.3.1 it gates on `tyc check` first (set `TYC_SKIP_CHECK=1` to bypass). See [RUNTIME.md](RUNTIME.md) for the supported feature surface. Programs that import CPython-only libraries fall back via `tyc run --compile`.

---

## 15. Python interop and `.dty` stubs

`.dty` is the Typhon stub format — strictly typed in the Typhon dialect (`T?`, `Result`, sealed unions, interfaces, `unsafe`). The compiler emits a PEP 561 `.pyi` companion so mypy / pyright / Pyrefly / `ty` understand it too.

A `.pyi` consumed *by* Typhon is treated as an `unsafe` boundary unless overridden by an authored `.dty`.

```python
# src/stubs/redis.dty
class Redis:
    host: str
    port: int

impl Redis:
    def get(self, key: str) -> str?
    def set(self, key: str, value: str) -> bool
    def delete(self, *keys: str) -> int
```

`tyc check --stubs` runs a Typhon port of mypy's `stubtest`: it diffs each `.dty`'s surface API against the runtime symbols of the module it describes and emits `tyc::stub_mismatch` for missing-in-impl / missing-in-stub / signature-mismatch findings. `tyc stubtest` runs the real `python -m mypy.stubtest` against every emitted `.pyi` (catches dynamically-created members the AST can't see — `__init_subclass__`, metaclass-driven member registration, Pydantic auto-generated fields).

**Cross-module shape extraction consumes both `.ty` and `.dty` on equal footing.** When both define the same name, stubs win.

### Layers of third-party type-checking (v0.12.0, + bundled stubs v0.15.0)

A third-party call can be type-checked several ways, in increasing coverage:

0. **Compiler-bundled `.dty` stubs (v0.15.0)** — `tyc` ships curated, embedded stubs for popular libraries whose packaging defeats venv introspection (`httpx`, `requests` to start). They're seeded into the project shape map by `tyc check` / `tyc build` / the LSP **before** venv introspection, so the library is shaped out of the box and its `unintrospectable-dependency` warning is suppressed — no `.venv` needed. Gap-fill only (an authored project `.dty`/`.ty` wins) and marked `partial` (omitted members stay lenient — no false `attribute_not_found`), so they add real construction checking + resolved client/response types without false positives on the long tail. Lives in `tyc-db::seed_bundled_stubs` with the `.dty` text under `tyc-db/src/bundled/`. The long tail stays best-effort; this is just the head.
1. **Authored `.dty` stub** — full Typhon-dialect types, the strongest and most precise. Write these for long-lived dependencies you call a lot. Overrides a bundled stub for the same module.
2. **Venv signature introspection (`tyc-venv`)** — `inspect.signature` over the *installed* package recovers parameter / return *annotations* (scalars, `Optional[X]` / `X | None`, parametric containers, fixed-arity tuples). Catches wrong-*type* and wrong-*arity* arguments to fully-typed pure-Python deps (function **and** constructor) with zero authoring. Degrades to a permissive `Unknown` on anything it can't model, so it only adds true positives. If a declared, imported dependency can't be introspected, `tyc::` surfaces the `unintrospectable-dependency` warning rather than silently skipping the check.
3. **`ty` typeshed pass (`[checker] external = "ty"` / `--with-ty`)** — the only path that sees **typeshed**, so it covers C-extension and stdlib APIs introspection can't reach (`os.path.join(1, 2)`, numpy/pandas signatures). Runs as a subprocess over the emitted Python, errors re-attributed to `.ty`.

Layer 0 is automatic for the bundled head; layer 1 is precise but manual; layer 2 is automatic for installed pure-Python deps; layer 3 is the catch-all for everything typeshed knows. They compose — an authored `.dty` wins on a name it defines, then the bundle, then venv, then `ty`.

> **Qualified ↔ bare class identity (v0.15.0):** a module names its own classes bare in signatures (`def get(...) -> Response`), so a qualified use-site reference (`import httpx; let r: httpx.Response = client.get(...)`, or `import lib; let x: lib.Foo = lib.make()`) unifies with the bare return type — `Checker::is_assignable` matches two class types by their final `.`-separated segment. This only relaxes assignability (a real `Response` vs `int` mismatch is still caught).

---

## 16. Compiler architecture

The whole pipeline lives in `tyc/crates/`, backed by a Salsa incremental DB.

```
.ty / .dty
    │
    ▼ tyc-syntax       lexer + parser (vendored Ruff fork — see tyc/vendor/)
    │                  let/mut soft-keywords with Mutability AST field
    │                  preprocess.rs expands T?, ?, |>, gather:, go, lazy, with-chains,
    │                  enum/model/class!/plain class headers
    ▼ tyc-db           Salsa queries: preprocessed_text/full, module_decl_names,
    │                  resolved_module, check_diagnostics, module_shapes_query
    ▼ tyc-resolve      name resolution + scope construction; enforces let/mut;
    │                  declaration sites; per-arm uninit tracking for v0.7.0 DA
    ▼ tyc-types        nominal types + non-null narrowing + structural conformance +
    │                  bidirectional generic inference; HKT scaffold (TypeConstructor)
    ▼ tyc-analyse      purity (6 conditions), async checks, comptime sandbox,
    │                  auto-gather data-flow, auto-parallel comprehension rewriter
    ▼ tyc-desugar      Typhon AST → Python AST (merge impl/extend, insert self,
    │                  expand ?, with-chains, gather:, go, pipes, lazy let, newtype,
    │                  freeze, pub, pub * aggregation)
    ▼ tyc-emit         Python codegen + .py.map v2 (per-statement out_line → ty_line)
    ▼ tyc-format       in-process whitespace pass + ruff format wrap
    ▼
    .py + .sourcemaps/*.py.map (+ generated typhon_runtime/ if used)
    │
    ▼ [checker] external = "ty"  (v0.12.0, opt-in / --with-ty)
                       run `ty` over the emitted .py; re-attribute diagnostics to .ty
```

- **`tyc-diagnostics`** uses miette/thiserror for the human-friendly format you see; every code carries a `url(https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/<code>.md)` deep-link.
- **`tyc-venv`** (v0.12.0) is the shared venv-introspection crate (`inspect.signature` over installed third-party packages → `VenvSignatures` of param/return types). Extracted from the `tyc` binary so both the CLI checker and `tyc-lsp` consume it. Powers third-party arity + argument-*type* checking and the `unintrospectable-dependency` warning.
- **`tyc-lsp`** is a `tower-lsp-server` backend reusing the same Salsa DB; serves diagnostics, hover, go-to-definition (cross-file via `resolved_module`), completion (including venv-driven member-access introspection cached per session), semantic tokens, "Remove unused import" code action, and (v0.12.0) live third-party arg/type diagnostics via a persistent `tyc-venv` cache.
- **`tyc-vm`** is the in-process tree-walking interpreter that powers `tyc run`. See [RUNTIME.md](RUNTIME.md).
- **`tyc/`** is the CLI binary that wires it all together with clap v4; subcommands live under `tyc/src/commands/`.
- **`tyc/vendor/`** holds the Ruff fork — `ruff_text_size`, `ruff_source_file`, `ruff_python_trivia`, `ruff_python_ast` (with the `Mutability` extension on assignment nodes), `ruff_python_parser`. See `tyc/vendor/README.md`.

When investigating a bug, the rule of thumb is:

| Symptom | Crate to start in |
|---|---|
| Parse error / wrong AST | `tyc/crates/tyc-syntax` (and vendored `ruff_python_parser`) |
| Wrong scope / unknown-name / let-reassignment confusion | `tyc-resolve` |
| Wrong type inference / nullable handling / generic binding | `tyc-types` |
| Wrong purity verdict / wrong async warning / wrong comptime result | `tyc-analyse` |
| Wrong lowering (e.g. `?` produced unexpected code) | `tyc-desugar` |
| Wrong Python output / source-map wrong line | `tyc-emit` |
| Diagnostic text / span wrong | `tyc-diagnostics` (and the call site that emitted it) |
| LSP hover / go-to-def / completion misbehaving | `tyc-lsp` |
| Wrong third-party arg/type check, or a spurious `unintrospectable-dependency` | `tyc-venv` (introspection) → `tyc-types` (the check) |
| `ty` / `--with-ty` integration, source-map re-attribution | `tyc/src/commands/ty.rs` (`run_ty_check`) + `tyc/src/commands/source_map.rs` |
| VM produces wrong answer | `tyc-vm` |
| CLI flag, exit code, watch loop | `tyc/src/commands/*.rs` |

The core pipeline is **strict** — every stage crate only depends on its upstream neighbours plus `tyc-diagnostics` and `tyc-db`; there are no skip-level dependencies. `tyc-venv` sits *beside* the pipeline: it's consumed by the `tyc` binary and `tyc-lsp` to enrich project shapes before `tyc-types` runs, not as a linear stage. The `ty` external checker runs *after* emit, as a subprocess.

---

## 17. The `typhon_runtime` package

`typhon_runtime/` is a **generated package** the build owns. It's written into the project's output dir on every `tyc build` whenever the desugar pass sets `needs_typhon_runtime = true` (i.e. the emitted code references `Ok`/`Err`/`Result`, `typhon_runtime.tasks.spawn`, `typhon_runtime.lazy.lazy_let`, `typhon_runtime.parallel.map_pure`, or `typhon_runtime.freeze.deep_freeze`).

| File | Surface |
|---|---|
| `__init__.py` | Re-exports `Ok`, `Err`, `Result` |
| `result.py` | `Ok`, `Err`, `Result` dataclasses + `.map`/`.map_err`/`.and_then`/`.or_else` methods (v0.6.0) |
| `tasks.py` | `spawn(coro)` with strong-ref `_BACKGROUND` set + done-callback `discard` |
| `lazy.py` | `_LazyModule` / `lazy_import(name)`; `_LazyValue` / `lazy_let(factory)` |
| `parallel.py` | `map_pure(fn, iterable)` — `concurrent.futures.ThreadPoolExecutor`-backed parallel map; degrades to sequential on GIL-locked CPython |
| `freeze.py` | `deep_freeze(obj)` — recursively replaces `list → tuple`, `dict → MappingProxyType`, `set → frozenset`; raises `TypeError` on un-freezable values at startup |
| `cast.py` (v0.14.0) | `checked_cast(value, tp)` — backs `EXPR as! TYPE`; recursively verifies the value's shape against `tp` (scalars, parametric containers, unions/`Optional`) and raises `TypeError` on a mismatch |
| `traceback.py` (v0.14.0) | `install()` — sets a `sys.excepthook` that rewrites uncaught-exception frames to `.ty` source via the `.py.map` sidecars; emitted only when `[emit] traceback-remap = true` |
| `stdlib.py` | Internal helpers used by lowering passes |

**The runtime is generated; users do not edit it and do not `pip install` it.** Regenerated on every `tyc build`. There is no PyPI package — every project ships its own copy alongside the emitted `.py`.

The VM exposes the same surface natively, so `from typhon_runtime import Ok, Err` and `typhon_runtime.tasks.spawn` work in `tyc run`.

See [RUNTIME.md](RUNTIME.md) for the VM's full feature surface and the runtime helper internals.

---

## 18. `.py.map` v2 and source-mapped debugging

Sidecar source map written alongside every emitted `.py` under `<out>/.sourcemaps/` (v0.6.1; legacy adjacent layout still readable). The hand-written `tyc-emit` printer records a `(out_line → ty_line)` mapping at line granularity.

Consumers:

- **`tyc trace`** — rewrites Python tracebacks to point at `.ty` source. Paths with spaces use a longest-candidate walk-left lookup (v0.5.0).
- **`tyc debug --break <ty-file>:<line>`** — translates the Typhon source location through `.py.map` and injects `-c "break build/main.py:N"` into the debugger session.
- **`tyc debug` source-mapping wrapper** — loads every `.py.map` at startup and overrides pdb's `do_list`, `do_where`, `format_stack_entry`, and `prompt` so the **entire** debugger UI reads `.ty` (v0.5.0).
- **`tyc ty`** — same loader as `tyc trace`; rewrites `ty`'s `*.py:LINE[:COL]` diagnostics to point at `.ty` (v0.5.0). `--raw` opts out.
- **`tyc lsp`** — cross-file go-to-definition across the `.ty` / `.py` boundary.

---

## 19. Diagnostics catalog (top tier)

The recurring diagnostic codes and what they actually mean. **See [DIAGNOSTICS.md](DIAGNOSTICS.md) for the exhaustive reference** (`tyc explain --list` prints all 90 codes the compiler ships as of v1.0.0-alpha.9) — what follows is the daily-driver subset.

| Code | Meaning | Fix |
|---|---|---|
| `tyc::missing_annotation` | Function has no `-> T` or param lacks annotation | Add an explicit type (`-> None` if it returns nothing) |
| `tyc::missing_binding_kind` | Local `=` without `let`/`mut` | Add `let` (default) or `mut` (if rebound) |
| `tyc::immutable_assign` | Reassigning a `let` binding | Change to `mut`, or extract a new `let` |
| `tyc::missing_initialiser` | `let NAME: T` written that is never assigned | Initialise inline, or assign on every non-diverging path |
| `tyc::use_of_uninitialised` | (v0.7.0) Read of `let NAME: T` on a path that didn't assign | Initialise inline, or assign in every non-diverging arm |
| `tyc::go_outside_async` | `go f(x)` reached from module-level code (directly or via a sync function called at module level) — no running event loop, `RuntimeError` at the call | Move the `go` into an `async def` driven by `asyncio.run(...)`, or `asyncio.run(f(x))` |
| `tyc::possibly_unbound` | (warning) Ordinary local read on a path that may not — or provably does not — assign it (`if` without `else`, after `del`, `except … as e` after the handler, loop target after a possibly-empty loop) | Assign a default before the branch / loop, or `return` in the arm that has nothing to assign |
| `tyc::no_block_shadow` | Inner `let`/`mut` would shadow an outer binding | Rename inner binding (no block scope in Python) |
| `tyc::pattern_shadows_outer` | `case Wrap(value):` against outer `let value` | Rename the capture (`case Wrap(inner):`) |
| `tyc::nullable_use` | Passing `T?` where `T` required | Narrow with `is None` / `guard` / early-return |
| `tyc::missing_await` | Sync context calling `async def` | Add `await` and make caller `async` |
| `tyc::async_without_await` (warn) | `async def` with no `await` inside | Drop `async` or await something |
| `tyc::blocking_in_async` (warn) | Direct call to known-blocking stdlib inside `async def` | `asyncio.to_thread(...)` or async-native client. Suppressed inside `unsafe:` |
| `tyc::manual_init` | `class` defines `__init__` | Remove it — constructor is generated |
| `tyc::method_in_class_body` (warn) | A `def` inside `class Name:` instead of `impl Name:` | Move into an `impl` block |
| `tyc::field_default_ordering` | (v0.7.0) Non-default field after defaulted one in `class` | Reorder, or use a factory |
| `tyc::frozen_assign` | Writing a field on a `frozen` class | Build a new instance |
| `tyc::frozen_inheritance_conflict` | (v1.0.0-alpha.2) `frozen` / non-`frozen` dataclass mixed across an in-module base | Make both `frozen` or both non-`frozen` |
| `tyc::not_a_context_manager` | (v1.0.0-alpha.2) `with` / `async with` over a local class lacking `__enter__`/`__exit__` (or `__aenter__`/`__aexit__`) | Add the protocol methods or use a `@contextmanager` factory |
| `tyc::raise_non_exception` | (v1.0.0-alpha.2) `raise 42` / raising a plain dataclass | Raise a `BaseException` subclass |
| `tyc::missing_field_init` | `X.__new__(X)` escape without every required field assigned | Use the regular constructor |
| `tyc::non_exhaustive_match` | `match` on a sealed union misses a variant | Add the missing `case` or use `case _:` |
| `tyc::invalid_question_op` | `?` inside a non-`Result` function | Change the signature or `match` explicitly |
| `tyc::result_error_mismatch` | `?` returns `Err[E1]` into `Result[T, E2]` | Convert at the boundary with `match` or `.map_err` |
| `tyc::impure_pure_fn` | `@pure` function fails one of the 6 conditions | Refactor or drop `@pure` |
| `tyc::interface_isinstance` | `isinstance(x, SomeInterface)` | Use static narrowing or refactor to a sealed union |
| `tyc::interface_not_conforming` | Type missing/incompatible interface members | Add the method or fix the signature |
| `tyc::stub_mismatch` | `.dty` vs runtime drift detected by `tyc check --stubs` | Update the stub or implementation |
| `tyc::type_mismatch` | Assignment / argument type mismatch. (v0.12.0) Also fires on wrong-typed args to fully-typed **third-party** function / constructor calls via venv introspection | Fix the value's type, or wrap/convert at the boundary |
| `tyc::unused_import` (warn) | Severity controlled by `[strictness] unused-import` (default `warn` since v0.8.0) | Remove the import (LSP "Remove unused import" code-action exists) |
| `tyc::orphan_py_import` (warn) | `.py` outside `src/` referenced from a relative import | Move under `src/` or use an absolute import |
| `tyc::stdlib_module_shadow` (warn) | (v0.6.0) `.ty` filename matches a Python 3.13 stdlib top-level module | Rename (e.g. `lang_types.ty`, `records.ty`) |
| `tyc::auto_gather_missed` (advice) | Adjacent awaits look gather-able but a callee lacks `@gatherable` | Decorate the named callee |
| `tyc::newtype_violation` | Bare base-type value flowing into a `newtype` slot or wrong-typed constructor arg | Wrap with the constructor: `UserId(raw_int)` |
| `tyc::resource_not_managed` (warn) | Bare assignment of `open` / `socket.socket` / `sqlite3.connect` / `tempfile.*` without `with` | Wrap in `with` or move into an explicit `try/finally`. Severity controlled by `[strictness] require-with` |
| `tyc::div_by_zero_literal` | Literal-divisor `/ 0`, `// 0`, `% 0` (including `-0.0` and unary-negated zero) | Fix the divisor or guard the call site |
| `tyc::unsafe_value_leak` | A `return x` outside the `unsafe:` block where `x` was declared | Re-assert inside (`let x: T = …`) or re-bind at the boundary (`let typed: T = x`) |
| `tyc::extend_builtin` | `extend list[int]:` (parametric target) | Drop the `[…]`; `extend list:` is the supported form |
| `tyc::duplicate_method` | Two `impl`/`extend` blocks define the same method | Rename, delete, or merge |
| `tyc::pub_name_collision` | (v0.7.0) Two siblings both `pub`-export the same name under `pub *` | Rename one, drop `pub` on one, or use explicit re-exports |
| `tyc::pub_star_outside_init` (advice) | (v0.7.0) `pub *` outside `__init__.ty` is a no-op | Move to `__init__.ty` or remove |
| `tyc::typevar_import_rejected` | `from typing import TypeVar` | Use PEP 695 (`def f[T](...)`) |
| `tyc::typing_alias_deprecated` | `from typing import List/Dict/...` | Use lowercase built-ins |
| `tyc::contains_secret_literal` (warn) | `comptime let *KEY/TOKEN/PASSWORD/SECRET = env(...)` would inline a secret | Read at runtime via `os.environ[...]` |
| `tyc::comptime_env_missing` (or via `tyc::comptime`) | Required env var unset at build time | Set the env var or remove from `[env] required` |
| `tyc::cyclic_type_alias` | `type A = B; type B = A` | Anchor one alias to a concrete type |
| `tyc::class_attr_shadows_slot` (warn) | `class` body with only annotated defaults reads like a constants namespace but emits slot descriptors | Use `ClassVar[T]`, or `pass` body for nullary variants |
| `tyc::tuple_index_out_of_range` | Constant index out of range for fixed-arity tuple | Use an in-range index or change to homogeneous tuple |

(v0.12.0) A declared dependency that's imported but can't be introspected surfaces the **`unintrospectable-dependency`** warning — severity via `[strictness] unintrospectable-dependency` (`"warn"` default / `"error"` / `"off"`). Install deps (`uv sync`) or ship a `.dty` to clear it.

`tyc explain <code>` and `tyc explain --list` work offline.

---

## 20. Common pitfalls (the ones every newcomer hits)

See [PITFALLS.md](PITFALLS.md) for the extended ranked list. The top tier:

1. **Forgetting `-> None`.** Sync functions returning nothing still need the annotation.
2. **Writing `x = 1` at function scope.** Locals require `let` or `mut`. (Module top-level is fine — defaults to `let`.)
3. **Calling `find_user(1)` and passing the result somewhere expecting `str`.** It's `str?`. Narrow first.
4. **Putting `def display(self) -> str` inside `class`.** Move to `impl ClassName:` and use `self.NAME` for fields.
5. **Writing `__init__`.** Don't. Use field defaults or a free function.
6. **`from typing import TypeVar`.** Use PEP 695: `def f[T](xs: list[T]) -> T?:`.
7. **`isinstance(x, MyInterface)`.** Rejected — use static narrowing or a sealed union.
8. **`asyncio.create_task(...)` for fire-and-forget.** Use `go f(x)`; the runtime registry holds a strong ref.
9. **`lazy from numpy import array`.** Rejected. Use `lazy import np = numpy` + `np.array(...)`.
10. **`comptime let NOW: float = time.time()`.** Sandbox forbids `time.*`. Compute at runtime with `lazy let`.
11. **`dict.get(k)` typed as `V`.** It's `V?`. Either narrow or use `d[k]`.
12. **Empty list with no annotation.** `let xs: list = []` is a missing-annotation error. Write `list[int]` or similar.
13. **Blocking I/O inside `async def`.** `tyc::blocking_in_async` catches `time.sleep`, `requests.*`, `subprocess.run`, etc.
14. **Expecting `bool` to NOT be `int`.** Since v0.4.0, `bool ⊆ int` — `let x: int = True` works. Reverse is still rejected.
15. **`f = open("x")` without `with`.** `tyc::resource_not_managed` flags this.
16. **`x / 0` literal divisor.** `tyc::div_by_zero_literal`.
17. **`fetch_user(42)` against `def fetch_user(id: UserId)` where `newtype UserId = int`.** Wrap explicitly: `UserId(42)`.
18. **`case value:` shadowing an outer `let value`.** `tyc::pattern_shadows_outer` — rename the capture.
19. **Filename matching a stdlib module (`types.ty`, `json.ty`, `io.ty`).** `tyc::stdlib_module_shadow` — rename.
20. **`pub *` in a non-`__init__.ty` module.** No-op + advice. Move to `__init__.ty`.
21. **Two siblings exporting the same `pub` name.** `tyc::pub_name_collision` under `pub *` aggregation.
22. **`let loaded: Cfg` then reading `loaded` on a path that didn't assign.** `tyc::use_of_uninitialised` (v0.7.0). Initialise inline or assign in every non-diverging arm.
23. **`class P: x: int = 0; y: int` (defaulted before non-defaulted field).** `tyc::field_default_ordering` (v0.7.0). Reorder.
24. **Nullary sealed-union variants written as `case Foo(_):`.** `_` is a positional capture for a class with no positional fields — never matches. Write `case Foo():` (two empty parens) and declare as `class Foo frozen: pass`.
25. **`model X frozen:`.** Doesn't parse — `frozen` is on `class` only.
26. **`let`-shadowing.** Rejected. Use `mut` or a fresh name; never re-bind with `let`.
27. **`lazy let X: T:` colon-block form.** Doesn't parse. Use `lazy let X: T = expr`.
28. **`lazy[list[T]]` return type.** Designed but not yet implemented — use `Iterator[T]`.
29. **Multi-line `|>` pipes without wrapping parens.** Wrap the whole chain in parens.

---

## 21. Recipes — minimum-viable patterns for common tasks

See [COOKBOOK.md](COOKBOOK.md) for canonical patterns extracted from the 68 example exercises and 15 production-shaped apps. Quick pointers:

| Task | Example |
|---|---|
| Hello world / argv | `examples/01-hello-world/` |
| `let` vs `mut` / `T?` narrowing | `examples/02-variables-and-types/` |
| Control flow / comprehensions | `examples/03-control-flow/` |
| Collections / dict.get / tuple destructure | `examples/04-collections/` |
| PEP 695 generics / `Callable` / closures | `examples/05-functions-and-generics/` |
| `class` / `class frozen` / `model` / `impl` / `extend` | `examples/06-classes-and-models/` |
| `Result` / `?` / `with`-chain | `examples/07-error-handling/` |
| `try_result` / `as!` at the untyped boundary | `examples/59-boundary-casts/` |
| `rescue` postfix + block exception boundaries | `examples/60-rescue-boundaries/` |
| Sealed unions + exhaustive match | `examples/08-sealed-unions-match/` |
| Structural interfaces | `examples/09-interfaces/` |
| Pipes + guards | `examples/10-pipes-and-guards/` |
| `comptime let` config from env | `examples/15-comptime-config/` |
| `model` + JSON load via `model_validate` | `examples/17-file-io-json/` |
| Subclassing stdlib (`logging.Formatter`) | `examples/20-logging/` |
| Argparse + sealed-union command + match dispatch | `examples/21-cli-tool/` |
| Async + Result | `examples/23-async-basics/` |
| `gather:` + `@gatherable` + `go` | `examples/24-async-gather-and-go/` |
| FastAPI server with `model` + DI | `examples/28-fastapi-server/` |
| `lazy import np = numpy` | `examples/29-numpy-arrays/` |
| `lazy import torch = torch` + `torch.Tensor` annotations | `examples/33-pytorch-tensors/` |
| Anthropic client (Result-wrapped) | `examples/38-llm-anthropic/` |
| Tool-use loop with `unsafe:` AST walker | `examples/40-llm-tool-use/` |
| Generic agent framework with `Callable` fields | `examples/43-agent-framework/` |
| Multi-file mini app (FastAPI + SQLite + Anthropic) | `examples/47-mini-app/` |
| `newtype` IDs across boundaries | `examples/48-newtype-ids/` |
| Generic sealed-union linked list | `examples/50-linked-list/` |
| `@contextmanager` factories | `examples/58-context-managers/` |
| JSON-RPC with newtype IDs + `unsafe:` boundary coercion | `examples/68-json-rpc-builder/` |
| Pytest + match on Result | `examples/testing/` |
| Production-shaped apps (15) | `examples/apps/01..15-*/` |

---

## 22. CI integration

`tyc check` is the CI-recommended primary gate — runs everything up to the analyser without emitting `.py`. Failure cases:

- Any `tyc::*` diagnostic at `"error"` severity.
- Required env var missing (`comptime let` fails when `DATABASE_URL` etc. is unset, if listed in `[env] required`).
- `tyc check --stubs` drift if you ship `.dty` stubs.

Recommended pipeline:

```yaml
- run: tyc check src/                  # primary gate
- run: tyc check --stubs               # if you ship .dty stubs
- run: tyc ty                          # second-opinion (needs `pip install ty`)
- run: tyc stubtest                    # runtime probe (needs mypy in the venv)
```

`tyc ty` runs Astral's `ty` checker against the emitted Python with `.ty` path attribution. `tyc stubtest` runs mypy's stubtest against the emitted `.pyi`.

---

## 23. Authoring Typhon code as Claude

When you edit `.ty` files in this repo or a downstream project:

1. **Read the relevant guide first** (`docs/guides/`) — every feature has a worked example and its emitted Python listed there. Cross-reference before guessing syntax.
2. **Annotate everything.** If `tyc check` would flag it, write the annotation. There is no implicit `Any` fallback.
3. **Reach for `let` before `mut`.** Only switch to `mut` when you actually need to rebind.
4. **Prefer `Result[T, E]` over `try/except`** anywhere errors are expected (parsing, lookups, validation). Use `try` only at the boundary into untyped libraries.
5. **Prefer sealed unions over inheritance** for closed sets of variants. They give you exhaustive `match`; subclassing doesn't.
6. **Methods go in `impl` blocks**, never inside the `class`. Explicit `self`; access fields via `self.NAME`.
7. **`extend` for cross-module method addition**, `extend BUILTIN:` for static-only built-in extensions.
8. **`gather:` only for genuinely independent awaits.** If one depends on another's value, leave them sequential.
9. **`go` for fire-and-forget**, never `asyncio.create_task` directly.
10. **`lazy import name = module`** for expensive optional deps; never `lazy from ... import ...`.
11. **`comptime let` for build-time constants** (especially required env vars). Don't put secrets there — use `os.environ[...]` at runtime.
12. **`@pure` only when the six conditions hold.** Mark `@memo` separately or use `@pure(memo=True)`. Never silently rely on `auto-memoise` for code others read.
13. **`unsafe:` is a *lexical* region.** Re-assert types at the boundary, don't smuggle `Unsafe[T]` outward. Always end the block with a re-assertion or an unreachable raise. For a *single* one-off boundary value, prefer **`EXPR as! TYPE`** (v0.14.0) — it's one line, needs no block, and is runtime-checked (so it's sound, unlike a bare re-assertion). Reach for `model X:` when you validate the same shape repeatedly, a `.dty` stub for a long-lived dependency.
14. **For multi-file projects, lean on `pub` + `pub *`.** See [PACKAGING.md](PACKAGING.md).
15. **After significant edits, run `tyc fmt src/ && tyc check src/`** and read the diagnostics. The checker is the source of truth.
16. **Read the emitted Python** for any non-trivial feature you haven't seen lower before (`tyc build` then look at `build/*.py`). The lowering is the spec.

When you edit the Rust compiler:

1. **Each diagnostic is registered once** in `tyc-diagnostics`. Search for the code (`rg "TYC_CODE_NAME" tyc/crates`) to find every site that emits it.
2. **Salsa queries** in `tyc-db` are the cache boundary; if a value should be incrementally tracked, it goes through a query.
3. **The vendored Ruff fork** under `tyc/vendor/` is on a clean branch and tracks upstream loosely; do not edit it without a clear note in `tyc/vendor/README.md`.
4. **Run `cargo test --workspace`** before pushing; the LSP tests use `tower-lsp-server`'s harness and the parser tests round-trip a corpus.

---

## 24. Quick reference — Typhon-specific syntax

| Feature | Syntax |
|---|---|
| Immutable local | `let x: int = 5` |
| Mutable local | `mut x: int = 0` |
| Declare-only let (v0.7.0) | `let loaded: Cfg` (assigned later on every non-diverging path) |
| Deep-immutable module binding | `freeze let CFG = {...}` |
| Public modifier | `pub def f(...) -> T: ...` |
| Package wildcard re-export | `pub *` in `__init__.ty` |
| Newtype | `newtype UserId = int` |
| Nullable type | `T?` (sugar for `Optional[T]`) |
| Generic fn (PEP 695) | `def f[T, U](x: T) -> U:` |
| Generic class (PEP 695) | `class Box[T]:` |
| HKT scaffold (v0.5.0) | `class Functor[F[_]]:` |
| Frozen class | `class Point frozen:` |
| Plain class (no decorator) | `plain class Bag:` |
| Raw class (framework base) | `class! MyModel(nn.Module):` |
| Enum (v0.11.0) | `enum Color: RED; GREEN; BLUE` |
| Methods | `impl Foo:` block separate from `class Foo:` |
| Cross-module method add | `extend Foo:` |
| Built-in extension | `extend str: def slug(self) -> str: ...` |
| Boundary type | `model X:` (Pydantic-backed) |
| Interface (structural) | `interface Drawable: def draw(self) -> None` |
| Sealed union | `type Shape = Circle \| Rectangle` |
| Distributed impl | `impl Shape: def area(self) -> float: ...` |
| Pattern match | `match s: case Circle(radius): ...` |
| Result success | `Ok(value)` |
| Result failure | `Err(error)` |
| Result propagate | `let x: int = parse()?` |
| Multi-Result chain | `with a = r1?, b = r2?: ... else err: ...` |
| Result combinators | `r.map(f) / r.map_err(g) / r.and_then(h) / r.or_else(k)` |
| Exception→Result | `try_result(lambda: f(), lambda e: str(e))` |
| Boundary rescue (postfix) | `let n: int = int(s) rescue e: f"bad: {e}"` |
| Boundary rescue (block) | `rescue e: ERR:` then an indented suite |
| Guard / early return | `guard u = maybe else: return default` |
| Pipe | `value \|> f() \|> g()` |
| Compile-time const | `comptime let PORT: int = int(env("PORT", "8080"))` |
| Compile-time fn | `comptime def feature(name: str) -> bool: ...` |
| Compile-time type | `comptime let T: type = int` |
| Deferred import | `lazy import np = numpy` |
| Deferred module-level | `lazy let CFG: Config = load()` |
| Concurrent await | `gather: a = f1(); b = f2()` |
| Best-effort gather | `gather(strategy="best-effort"): ...` |
| Fire-and-forget | `go background_task(x)` |
| Capture handle | `go background_task(x) -> task` |
| Gather marker | `@gatherable async def ...` |
| Pure assertion | `@pure def f(...) -> T:` |
| Memoised | `@memo def fib(n: int) -> int:` |
| Type-check escape | `unsafe: ...` (followed by re-assertion or unreachable raise) |
| Checked boundary cast | `let d = resp.json() as! dict[str, int]` |
| Entry guard | `if __name__ == "__main__": main()` |

---

## 25. Further reading

Inside this repo:

- **`docs/long-term-plan.md`** — the canonical design doc.
- **`docs/architecture.md`** — pipeline + crate-by-crate breakdown.
- **`docs/vm.md`** — VM feature surface and design rationale.
- **`docs/prior-art.md`** — TypeScript, rust-analyzer, ty, Pyrefly, oxc, Ruff influence.
- **`docs/risks.md`** — what we expect to bite us.
- **`docs/roadmap.md`** — phased delivery (Phase 6 — Python-annoyances surface complete).
- **`docs/findings.md`** — consolidated stress-test findings.
- **`docs/ty-integration.md`** — how `tyc ty` cooperates with Astral's checker.
- **`docs/performance-baseline.md`** — measured numbers.
- **`TYPE_SYSTEM_FRONTIER.md`** — remaining open frontier work (embedded `ty` Phase 2, accumulator-loop parallelisation, typeshed pure-extension checking). HKT unification, user-generic variance inference, and the inter-procedural field-init audit landed in v1.0.0-alpha.
- **`CHANGELOG.md`** — every release.
- **`stress/`** — stress-test corpora (multi-round campaigns).
- **`tyc/vendor/README.md`** — Ruff fork rationale.
- **`editors/vscode/README.md`** — reference VS Code extension.

Sibling files in this skill:

- **[REFERENCE.md](REFERENCE.md)** — every syntactic form with emitted Python.
- **[CLI.md](CLI.md)** — verbose subcommand reference.
- **[PITFALLS.md](PITFALLS.md)** — extended pitfalls catalogue.
- **[DIAGNOSTICS.md](DIAGNOSTICS.md)** — exhaustive `tyc::` code catalog.
- **[COOKBOOK.md](COOKBOOK.md)** — canonical patterns from `examples/`.
- **[RUNTIME.md](RUNTIME.md)** — the generated `typhon_runtime` package and the in-process VM.
- **[PACKAGING.md](PACKAGING.md)** — multi-file projects, `__init__.ty`, `pub *` aggregation.
