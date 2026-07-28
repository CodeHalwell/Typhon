# Typhon v1.0.0-alpha.6 — Full Codebase Review (2026-07-28)

**Method:** a 103-agent workflow — 26 domain reviewers across the compiler pipeline, VM, tooling,
docs and corpus; each reviewer's top findings independently re-tested by an adversarial verifier
prompted to *refute*; then a coverage critic and a senior synthesis pass.
**Reviewed commit:** `7cf612f` · **Branch:** `claude/typhon-codebase-review-workflow-gn7z55`
**Binary under test:** release `tyc 1.0.0-alpha.6`, CPython 3.13 · **Cost:** 14.8M tokens, 2,473 tool calls, ~4.3h.

---

## Results at a glance

| | |
|---|---|
| Domains reviewed | 26 of 26 |
| Raw findings reported | 125 |
| Findings put through adversarial verification | 75 (top 3 per domain) |
| **Survived verification** | **74** (67 CONFIRMED, 7 PARTIAL) |
| Refuted | 1 |
| Unverified leads (reported, never tested) | 55 |
| Verified severity mix | 4 CRITICAL · 48 HIGH · 18 MEDIUM · 4 LOW |
| Root-cause clusters identified | 12 (+1 meta) |

---

## How much to trust this

Read these caveats before acting on the findings.

**1. Verification was genuinely execution-grounded — I checked.** A 95% confirmation rate looks
like rubber-stamping, so the verifier transcripts were audited directly: **620 Bash invocations
across 43 verifier agents** (7–23 each), **42 of 43 ran the real `tyc` binary**, and **42 of 43
ran `python3.13`**. Verifiers reproduced findings independently rather than agreeing on sight.

**2. Severities are verifier-corrected, and the correction was large.** Reviewers claimed 19
CRITICAL among their top findings; verifiers sustained **4**. Treat the severity column as
calibrated, and reviewer-assigned severity in the unverified-leads appendix as *not* calibrated.

**3. Only the top 3 findings per domain were verified.** The 55 leads in Appendix B were reported
by a reviewer and never independently tested. They are hypotheses, not findings.

**4. One domain produced nothing.** The `tyc-venv` reviewer returned the literal string `Test.` —
it no-opped. That crate spawns Python subprocesses and hand-parses annotation strings straight
into `tyc::type_mismatch`, and the coverage critic rates it the single worst gap in the review
(the prior 50-agent audit never covered it either). A replacement review was commissioned
separately.

**5. Coverage is not complete, and the critic quantifies where.** ~5,700 lines of first-party
production Rust were cited by no reviewer, and the 53,337-line vendored Ruff fork was read by
nobody in either audit. Full accounting in the coverage critique below.

**6. Duplicates against prior audits were screened.** Verifiers checked each finding against the
logged-pending lists in `docs/adversarial-audit-50agent-2026-06-28.md`, `docs/findings.md`,
`RELEASE_READINESS_REVIEW.md` and `TYPE_SYSTEM_FRONTIER.md`, and duplicates were refuted.

---

## Verified findings — index

Full mechanism, impact and suggested fix for each:
**[codebase-review-2026-07-28-findings.md](codebase-review-2026-07-28-findings.md)**.

| ID | Sev | Category | Domain | Site | Finding |
|---|---|---|---|---|---|
| F1 | CRITICAL | emit-runtime-crash | emit-sourcemap | `tyc/crates/tyc-emit/src/printer.rs:1074` | Emitter never parenthesises operands of `Expr::Compare` or `Expr::If`, re-emitting differently-parsing Python: silent wrong results and a hard SyntaxError from a clean build |
| F2 | CRITICAL | soundness | types-generics | `tyc/crates/tyc-types/src/lib.rs:10301` | Instance-attribute assignment (`obj.field = v`, `self.field = v`) is never type-checked — any wrong-typed value silently corrupts a declared field |
| F3 | CRITICAL | emit-runtime-crash | types-result | `tyc/crates/tyc-syntax/src/preprocess.rs:6022` | Inline `?` in an `elif` condition is lifted between the `if` body and the `elif`, silently reattaching the `elif` to the generated `if isinstance(..., Err)` — wrong branch taken and spurious `Err` returned |
| F4 | CRITICAL | vm-parity | vm-stdlib | `tyc/crates/tyc-vm/src/builtins.rs:1352` | VM str/list/tuple search methods silently discard the start/end positional arguments (find, rfind, index, rindex, count, startswith, endswith), producing wrong results and non-terminating scan loops |
| F5 | HIGH | vm-parity | analyse-async | `tyc/crates/tyc-vm/src/builtins.rs:4057` | `gather:` task failure: VM raises the bare exception (catchable), compiled CPython raises an uncaught ExceptionGroup — clean check, opposite outcomes |
| F6 | HIGH | soundness | analyse-async | `tyc/crates/tyc-syntax/src/preprocess.rs:7557` | String-blind dependency scan silently demotes a whole `gather(strategy="best-effort")` block to sequential awaits, turning captured per-task errors into a crash |
| F7 | HIGH | emit-runtime-crash | analyse-async | `tyc/crates/tyc-emit/src/printer.rs:679` | `except*` is accepted and type-checked but never validated or modelled: `return` inside an `except*` body emits Python that fails with SyntaxError, and the VM binds the bare exception instead of a group |
| F8 | HIGH | soundness | analyse-comptime | `tyc/crates/tyc-analyse/src/lib.rs:3201` | Purity walker ignores comprehensions, f-strings, lambdas and dict/set literals, so `@pure`/`@memo` accept entropy and clock reads and inject `@functools.cache` on nondeterministic functions |
| F9 | HIGH | soundness | analyse-comptime | `tyc/crates/tyc-analyse/src/lib.rs:1110` | comptime `int(float)` uses Rust's saturating `as i64` cast, folding a wrong integer constant for any float outside i64 range (and silently converting `inf`/`nan` instead of raising) |
| F10 | HIGH | emit-runtime-crash | analyse-perf | `tyc/crates/tyc-analyse/src/reductions.rs:243` | auto-parallel-reductions deletes the for statement without checking whether the loop target is read after the loop, so emitted Python raises NameError |
| F11 | HIGH | performance | analyse-perf | `tyc/crates/tyc/src/commands/build.rs:1017` | auto-parallel is not gated on free-threading and the documented sys._is_gil_enabled() sequential fallback does not exist, making tyc build -O a ~60x pessimisation on stock CPython |
| F12 | HIGH | emit-runtime-crash | cli-build | `tyc/crates/tyc/src/commands/build.rs:1211` | A hand-written src/X.py silently overwrites the emitted build/X.py compiled from src/X.ty, so the shipped program is not the program tyc check validated |
| F13 | HIGH | architecture | cli-build | `tyc/crates/tyc/src/commands/deps.rs:232` | [python] target = 3.13t / 3.14t / 3.15t generates requires-python = ">=3.13t", an invalid PEP 440 specifier that permanently breaks uv sync, .venv creation, ruff format and third-party introspection |
| F14 | HIGH | crash-panic | cli-migrate | `tyc/crates/tyc/src/commands/migrate.rs:787` | tyc migrate injects `let`/`mut` into continuation lines of multi-line signatures and calls, producing .ty files that do not parse (7 of 10 real stdlib modules) |
| F15 | HIGH | crash-panic | cli-migrate | `tyc/crates/tyc-format/src/lib.rs:748` | tyc fmt splits an unparenthesized walrus `:=` into `: =`, destroying a valid, running program in place |
| F16 | HIGH | vm-parity | corpus | `tyc/crates/tyc-vm/src/interp.rs:462` | VM cannot resolve relative imports inside a sub-package: `tyc run` fails on 15/15 example apps while `tyc build` + CPython succeeds |
| F17 | HIGH | vm-parity | corpus | `tyc/crates/tyc-vm/src/interp.rs:865` | Every defaulted field on a `model` class is dropped from the VM constructor, so `Model(...)` and `Model.model_validate(...)` raise TypeError under `tyc run` but work under CPython |
| F18 | HIGH | emit-runtime-crash | desugar | `tyc/crates/tyc-desugar/src/lib.rs:818` | normalise_quoted_annotation_nullability rewrites ANY string literal ending in `?` inside an annotation, silently corrupting `Literal["?"]` into `Literal[" \| None"]` |
| F19 | HIGH | soundness | desugar | `tyc/crates/tyc-desugar/src/lib.rs:1855` | A user-defined or imported decorator named `memo` / `pure` / `gatherable` is silently stripped — and `@memo` is silently REPLACED by `@functools.cache` |
| F20 | HIGH | soundness | desugar | `tyc/crates/tyc-desugar/src/lib.rs:4017` | inherit_parent_fields clones the parent's field *initialiser expression* into every subclass — double-evaluating side-effecting defaults and forking `ClassVar` shared state |
| F21 | HIGH | soundness | emit-sourcemap | `tyc/crates/tyc/src/commands/build.rs:1085` | `.py.map` line table is captured before `ruff format` reflows the buffer, so `tyc trace` remaps every frame after the first reflowed line to the wrong `.ty` line |
| F22 | HIGH | vm-parity | emit-sourcemap | `tyc/crates/tyc-emit/src/printer.rs:1150` | f-string interpolation of a brace-opening expression emits `{{...}}`, which CPython reads as escaped literal braces — the expression is never evaluated |
| F23 | HIGH | diagnostics | lsp | `tyc/crates/tyc-lsp/src/lib.rs:1625` | Line-count-changing preprocessor expansions (`gather:`, `with`-chains) shift every LSP position below them — diagnostics publish past EOF and goto/hover stop resolving |
| F24 | HIGH | crash-panic | lsp | `tyc/crates/tyc-lsp/src/lib.rs:3110` | "Remove unused import" quick-fix deletes the wrong line, silently removing a used import |
| F25 | HIGH | soundness | resolve | `tyc/crates/tyc-resolve/src/lib.rs:1052` | `tyc::immutable_assign` is disabled for every binding declared inside any loop body — `for …: let x = i; x = 99` compiles clean |
| F26 | HIGH | crash-panic | robustness | `tyc/crates/tyc-types/src/lib.rs:3205` | Mutually-recursive parametric type aliases cause infinite recursion in Checker::is_assignable — tyc check/build/run abort with SIGABRT on a 5-line file |
| F27 | HIGH | performance | robustness | `tyc/crates/tyc/src/commands/util.rs:185` | Project file collector follows symlinks with no visited-path guard: a symlink cycle under src/ makes `tyc check` hang indefinitely; a single back-link duplicates every diagnostic 41x |
| F28 | HIGH | emit-runtime-crash | runtime | `tyc/crates/tyc/src/commands/build.rs:3143` | `EXPR as! SomeInterface` always raises TypeError inside checked_cast — a structurally valid cast to an `interface` is impossible on both the compiled and VM paths |
| F29 | HIGH | soundness | runtime | `tyc/crates/tyc/src/commands/build.rs:3147` | `EXPR as! SomeNewtype` is completely unchecked at runtime on the compiled path (checked_cast falls through to `return True`), while the VM rejects it — soundness hole plus VM/CPython divergence |
| F30 | HIGH | performance | runtime | `tyc/crates/tyc/src/commands/build.rs:3405` | `map_pure` has no runtime length or GIL guard — an auto-parallel-rewritten comprehension over a short runtime-length list ran 424× slower, and the documented `sys._is_gil_enabled()` sequential fallback does not exist in the generated runtime |
| F31 | HIGH | security | security | `tyc/crates/tyc-lsp/src/venv_introspect.rs:111` | `tyc lsp` executes arbitrary code from any module named in an open .ty file — TYC_NO_INTROSPECT does not disable it and there is no dependency allow-list |
| F32 | HIGH | emit-runtime-crash | security | `tyc/crates/tyc-emit/src/printer.rs:1926` | Emitter's triple-quoted-string escaper ignores a quote run at the tail, so an ordinary .ty string literal either silently loses characters (VM/CPython divergence) or emits Python that fails to parse |
| F33 | HIGH | emit-runtime-crash | syntax-preprocess | `tyc/crates/tyc-syntax/src/preprocess.rs:361` | `enum` body member rewrite runs on lines inside triple-quoted strings, silently mutating string constants and docstrings |
| F34 | HIGH | emit-runtime-crash | syntax-preprocess | `tyc/crates/tyc-syntax/src/preprocess.rs:7334` | `with`-chain body re-indentation strips leading whitespace from triple-quoted string content, silently changing string values |
| F35 | HIGH | false-positive | syntax-preprocess | `tyc/crates/tyc-syntax/src/preprocess.rs:6933` | Indent-only block collection in `collect_chain` and `expand_impl_sealed_unions` is blind to triple-quoted strings, rejecting valid programs with bogus `tyc::parse` errors |
| F36 | HIGH | emit-runtime-crash | types-classes | `tyc/crates/tyc-desugar/src/lib.rs:4007` | Multiple-inheritance constructor field order uses base declaration order, not CPython's reverse-MRO — silently scrambles positional constructor arguments and can crash at import |
| F37 | HIGH | emit-runtime-crash | types-classes | `tyc/crates/tyc-types/src/lib.rs:8999` | `model` (Pydantic) constructors accept positional arguments — `BaseModel.__init__` is keyword-only, so a clean check emits a guaranteed TypeError |
| F38 | HIGH | false-positive | types-core | `tyc/crates/tyc-types/src/lib.rs:3341` | `Checker::is_assignable` has no Union/Union arm — `Dog?` is not assignable to `Animal?` (nor `Button?` to `Drawable?`, `Circle?` to a sealed `Shape?`, `UserId?` to `int?`) |
| F39 | HIGH | soundness | types-core | `tyc/crates/tyc-types/src/lib.rs:378` | READ_VIEW_HEADS over-approximates the abc lattice: `set`/`frozenset` satisfy `Sequence[T]`/`Reversible[T]` and `list`/`tuple`/`set` satisfy `Iterator[T]` — clean check, `TypeError` at runtime in both CPython and the VM |
| F40 | HIGH | false-positive | types-core | `tyc/crates/tyc-types/src/lib.rs:9232` | `Type::Function` records no required-arity, so a function with a defaulted parameter is rejected where `Callable[[...], R]` is expected |
| F41 | HIGH | soundness | types-flow | `tyc/crates/tyc-types/src/lib.rs:10940` | Global flow-narrowing is invalidated by a call only in Assign/AnnAssign/Expr statements — a call in an if/while/for/return/assert/aug-assign position keeps a stale narrowing |
| F42 | HIGH | soundness | types-flow | `tyc/crates/tyc-types/src/lib.rs:15975` | Nullable-receiver check fires only for `Expr::Name` receivers: `b.inner.name` / `b.val.upper()` on a `T?` field is accepted with no diagnostic at all |
| F43 | HIGH | soundness | types-flow | `tyc/crates/tyc-types/src/lib.rs:9919` | Loop-body narrowing widening covers only bare-Name assignment targets — attribute narrowings, tuple-unpack targets, and globals mutated by an in-loop call stay stale at iteration 2 |
| F44 | HIGH | soundness | types-generics | `tyc/crates/tyc-types/src/lib.rs:11166` | `tyc::non_exhaustive_match` is silently skipped for every *parametric* sealed union — `type U[T] = A \| B \| C` loses exhaustiveness checking entirely |
| F45 | HIGH | soundness | types-generics | `tyc/crates/tyc-types/src/lib.rs:15464` | Generic call inference union-widens a type parameter that occurs in an *invariant* position, letting a `list[str]` bind the same `T` as a `list[int]` and write a `str` into a `list[int]` |
| F46 | HIGH | emit-runtime-crash | types-result | `tyc/crates/tyc-syntax/src/preprocess.rs:6055` | Inline `?` in a `while` condition is hoisted out of the loop, so the fallible expression is evaluated once and the loop condition is frozen — wrong result or infinite loop |
| F47 | HIGH | emit-runtime-crash | types-result | `tyc/crates/tyc-syntax/src/preprocess.rs:4413` | Postfix `rescue` over an `await` operand emits `try_result(lambda: await …)` — a hard CPython SyntaxError from a clean `tyc check` and an exit-0 `tyc build`, while the VM runs it correctly |
| F48 | HIGH | vm-parity | vm-core | `tyc/crates/tyc-vm/src/value.rs:1478` | int(float), math.floor/ceil/trunc saturate at i64::MAX/MIN — any \|float\| >= 2^63 silently converts to the wrong integer |
| F49 | HIGH | vm-parity | vm-core | `tyc/crates/tyc-vm/src/builtins.rs:780` | sum(iterable, start) silently ignores the `start` argument — returns a wrong total with no error |
| F50 | HIGH | vm-parity | vm-core | `tyc/crates/tyc-vm/src/interp.rs:1962` | value_cmp treats every incomparable value pair as Equal, so sorted()/min()/max() silently return wrong (unsorted) results for bytes and for mixed-type sequences |
| F51 | HIGH | emit-runtime-crash | vm-stdlib | `tyc/crates/tyc-format/src/lib.rs:519` | tyc-format rewrites the CONTENTS of triple-quoted string literals (leading tabs -> 4 spaces, trailing whitespace stripped), silently corrupting string values in emitted Python and in .ty source under tyc fmt |
| F52 | HIGH | vm-parity | vm-stdlib | `tyc/crates/tyc-vm/src/builtins.rs:1407` | VM isinstance(True, int) is False and isinstance(x, object) is False - is_instance_of lacks the bool-subtype-of-int and object arms, silently changing control flow |
| F53 | MEDIUM | soundness | analyse-perf | `tyc/crates/tyc-analyse/src/reductions.rs:320` | auto-parallel-reductions loses all partial accumulation when the element expression raises, and the module doc's exception-preservation claim is wrong |
| F54 | MEDIUM | crash-panic | cli-migrate | `tyc/crates/tyc/src/commands/migrate.rs:235` | tyc migrate over-indents the fields it synthesises when stripping a trivial __init__ from a class that has a docstring, emitting an unparseable class body |
| F55 | MEDIUM | false-positive | db-salsa | `tyc/crates/tyc-db/src/lib.rs:1090` | `check_impl` (the tracked `check_diagnostics` path) skips comptime-literal substitution, so `tyc repl` and the LSP's single-file mode emit `tyc::type_mismatch` false positives on code `tyc check` accepts |
| F56 | MEDIUM | performance | db-salsa | `tyc/crates/tyc-db/src/lib.rs:576` | The `module_shapes_query` Salsa cache is completely defeated in the LSP: `set_text` does not compare values in salsa 0.26, so every sibling module is re-preprocessed/re-parsed/re-extracted on every keystroke (412 ms/keystroke on a 100-module project) |
| F57 | MEDIUM | false-positive | db-salsa | `tyc/crates/tyc-lsp/src/lib.rs:1852` | The cross-module shape registry reads sibling modules from disk instead of the live `SourceFile` inputs the LSP already holds, so an unsaved edit in one module produces false errors in another |
| F58 | MEDIUM | false-positive | diagnostics | `crates/tyc-types/src/lib.rs:1552` | `List[int]` / `typing.List[int]` annotations produce a hard `tyc::type_mismatch` false positive against `list[int]`, with a nonsensical suggested fix |
| F59 | MEDIUM | diagnostics | diagnostics | `crates/tyc/src/commands/util.rs:76` | `[strictness] unused-import = "off"` is validated as a legal value but silently does nothing — the warning still fires |
| F60 | MEDIUM | architecture | diagnostics | `crates/tyc-lsp/src/lib.rs:437` | The LSP ignores every `[strictness]` severity knob — a project that turned a diagnostic off in `typhon.toml` still gets it squiggled in the editor |
| F61 | MEDIUM | docs-drift | docs-drift | `tyc/crates/tyc-emit/src/printer.rs:711` | PEP 810 `lazy import MODULE [as ALIAS]` type-checks clean but emits an eager import — the `lazy` marker is silently dropped by tyc-emit, and `tyc cheatsheet` teaches exactly this form |
| F62 | MEDIUM | docs-drift | docs-drift | `docs/cheatsheet.md:171` | The `## Concurrency` block in docs/cheatsheet.md — shipped verbatim by `tyc cheatsheet` — does not parse: `gather:` bindings are shown with `let` and `go`, neither of which the grammar accepts |
| F63 | MEDIUM | docs-drift | docs-drift | `docs/diagnostics/frozen_assign.md:10` | Prefix `frozen class X:` / `pub frozen class X:` is taught in four docs including the page `tyc explain frozen_assign` serves — the grammar only accepts the postfix `class X frozen:` |
| F64 | MEDIUM | false-positive | lsp | `tyc/crates/tyc-lsp/src/lib.rs:233` | A workspace path containing a space (or any percent-encoded character) silently disables all cross-module checking, venv introspection and `[strictness]` config in the LSP |
| F65 | MEDIUM | soundness | resolve | `tyc/crates/tyc-resolve/src/lib.rs:1736` | `global` / `nonlocal` silently defeats `let` immutability — a `let`/`freeze let` binding can be reassigned from another scope with no diagnostic |
| F66 | MEDIUM | false-positive | resolve | `tyc/crates/tyc-resolve/src/lib.rs:1530` | `from module import *` — a documented, supported form — is a hard `tyc::unknown_name` false positive on every star-imported name, blocking the build |
| F67 | MEDIUM | test-gap | tests-ci | `tyc/crates/tyc/tests/pipeline.rs:1470` | Every `.py.map` source-map test passes with a totally wrong map — no test asserts a single out_line→ty_line mapping is correct |
| F68 | MEDIUM | test-gap | tests-ci | `tyc/crates/tyc/tests/build_features.rs:21` | All 14 emitted-Python-execution and REPL tests silently no-op when `python3` is off PATH, and the CI test job never installs Python |
| F69 | MEDIUM | performance | tests-ci | `scripts/perf-gate.sh:49` | The CI perf gate spends ~55-59% of its measured wall-clock in a venv-introspection Python subprocess, so its effective threshold on compiler work is ~45%, not 20% |
| F70 | MEDIUM | emit-runtime-crash | types-classes | `tyc/crates/tyc-desugar/src/lib.rs:3163` | `class!` synthesised `__init__` treats a `ClassVar[T]` field as a constructor parameter and strips its class-level default — the class attribute ceases to exist, AttributeError on a clean check |
| F71 | LOW | crash-panic | cli-build | `tyc/crates/tyc/src/commands/build.rs:1130` | tyc build writes every artifact with non-atomic std::fs::write; an interrupted build leaves a persistent 0-byte build/main.py that CPython runs successfully with exit 0 |
| F72 | LOW | false-positive | corpus | `tyc/crates/tyc-types/src/lib.rs:10327` | Additive-compatibility violation shipped in alpha.2: the new subscript-assignment check rejects an `object`-typed RHS, breaking a stress fixture that previously checked clean and ran correctly |
| F73 | LOW | crash-panic | robustness | `tyc/crates/tyc-types/src/lib.rs:14665` | tyc-types::infer_expr_ctx recurses with no depth guard — a large machine-generated expression aborts the compiler with a stack overflow instead of emitting a diagnostic |
| F74 | LOW | diagnostics | security | `tyc/crates/tyc-analyse/src/lib.rs:3544` | `tyc::contains_secret_literal` misses first-tier secret name shapes (PASSPHRASE, CREDENTIALS, DSN, COOKIE, AUTHORIZATION, WEBHOOK), and the PASS boundary rule that excludes PASSPORT also excludes PASSPHRASE |

---

# Typhon v1.0.0-alpha.6 — Senior Synthesis & Action Plan

74 verified findings. Below they are collapsed into **12 root causes**, one of which explains a third of them.

---

## 1. Root-cause clustering

### Cluster 0 (meta) — *The same knowledge is derived independently in two or more places, and only one copy gets updated*

This is not a cluster of findings; it is the shape of the codebase. It explains, wholly or partly, **F7, F8, F9, F16, F17, F18, F31, F32, F40, F41, F42, F44, F48, F53, F63, F70** — 16 findings. Enumerated duplications, all confirmed in source:

| # | Derivation | Copies | Findings |
|---|---|---|---|
| 1 | Assignability traversal | `assignable()` (types:274) + `Checker::is_assignable` (types:3045) | F7, F8 |
| 2 | Class field order / ctor arity | tyc-types `effective_class_shape` (8921) + tyc-desugar `inherit_parent_fields` (4007) + tyc-vm `collect_mro_fields` (interp:932) | F16, F32, F70 |
| 3 | `is_classvar_annotation` | tyc-types (13753); **zero occurrences of "ClassVar" in tyc-desugar** | F18 |
| 4 | Venv interpreter discovery | `tyc_venv::discover_python` (gated) + `tyc-lsp/venv_introspect.rs` (ungated) | **F63** |
| 5 | `apply_strictness` | `tyc/src/commands/util.rs` only; LSP has no copy | F53, F52 |
| 6 | Check pipeline | `check_impl` + `check_source_file_with_imports` | F48 |
| 7 | "does value match type name" (VM) | `is_instance_of` + `value_matches_cast_type` + `pattern_match_class` | F44, F37 |
| 8 | `sum` positional vs kwargs path | two impls, one with a comment asserting the other works | F40 |
| 9 | `Type::Function` construction | 3 sites disagree on `variadic` | F9 |
| 10 | Per-pass string-state scanners | ~6 partial copies in preprocess.rs | Cluster A |
| 11 | Decorator marker matching | tyc-desugar `is_purity_marker` + tyc-analyse `decorator_intent`, both bare string compare | F31 |

`SECRET_NAME_KEYWORDS` (alpha.6) is the one place this was fixed — and it was fixed *after* the table drifted twice. That is the template.

---

### Cluster A — Preprocessor decides block/statement identity from whitespace, with per-pass partial string/bracket awareness
**Explains: F1, F2, F3, F19, F20, F21, F23, F43, F57, F58, F66 (11)**

`preprocess.rs` is a 12,508-line line-oriented rewriter. Passes that got a shared, correct mask (`compute_code_skip_mask` for `as!`/`rescue`) survived adversarial probing. Passes that hand-rolled their own state did not:

- **enum body** (`preprocess_opts`:361) — membership by indent only; all three `continue`s bypass the sole `in_string` advancement at :925.
- **with-chains** (`collect_chain`:6933 / `render_chain`:7334) — boundary test and re-indentation both string-blind.
- **sealed-union impl** (`expand_impl_sealed_unions`:1902) — probe loop has *no* `scan_line_code_end` call at all, while the outer loop does.
- **inline `?`** (`expand_inline_question_ops`:5994) — lifts statements "above the physical line" with no notion of compound headers or loop bodies.
- **rescue** (`try_rewrite_rescue_at`:4413) — splices the operand into a `lambda:` without scanning for `await`.
- **gather dependency scan** (`expr_references_identifier`:7557) — raw byte substring; its own doc comment concedes the hole and misjudges its likelihood.
- Same disease outside the crate: `tyc-format` (F43, F58) and `tyc migrate` (F57, F59).

Consequence profile is the worst in the report: **silent mutation of string constants that survives `tyc check`, `tyc build`, *and* the VM/CPython cross-check** (F1, F2, F43), plus false-positive rejections of valid programs (F3, F57, F58).

---

### Cluster B — Three stages change line counts; none produces a line map
**Explains: F45, F46, F34, F72 + pending [59] (4+1)**

`PreprocessResult` carries `line_col_shifts` (**columns only**). `expand_gather_blocks` / `expand_with_chains` insert lines. Desugar injects imports. `format_source` (ruff) reflows the buffer **after** `line_offsets` was captured (build.rs:1085 → 1096). The LSP's own helpers assert in their doc comments that preprocessing "never adds or removes lines" — false.

Result: diagnostics published past EOF, goto/hover dead below any expansion, a buffer-mutating quick-fix that deletes the wrong line, and 35/35 stale sourcemaps across three shipped example apps. F72 is why this survived: **no test asserts a single mapping value** — the only assertion is `ty_line >= 1`, which `partition_point(...) + 1` makes vacuous.

---

### Cluster C — Assignability is duplicated, and applied at hand-enumerated *positions* rather than through one assignment abstraction
**Explains: F7, F8, F9, F10, F12, F71 (6)**

Two sub-causes:

- **C1 (duplication)**: the free layer carries an explicit comment warning that Union/Union must precede the single-Union arms; the nominal layer was written without heeding it (F7). The READ_VIEW table is spelled twice (F8) — patching one leaves the hole open.
- **C2 (position enumeration)**: `check_stmt`'s `Stmt::Assign` arm dispatches on target shape and handles `Name`, `Subscript`, `Tuple`/`List` — and **forgets `Attribute` entirely** (F10). `Stmt::AugAssign` *does* cover attribute targets, proving this is an omission, not a policy. Same shape at the call site: arguments are checked against un-substituted formals with no post-inference re-validation (F12).

**F10 is the single most severe finding in the report**: `self.field = v` — the mutation idiom Rule 4 *mandates* — is never type-checked, and the bad write then narrows the attribute so it launders the field's type and cancels a diagnostic that otherwise fires.

---

### Cluster D — Cross-cutting obligations re-implemented per statement arm
**Explains: F13, F14, F15 (3) + a status correction**

`reset_global_narrowings` is called from **3 of ~15** statement arms. Loop widening walks **bare-`Name` targets only**. `assign_unpacking_target` never narrows or clears. The nullable-receiver guard is gated on `if let Expr::Name` purely because `nullable_use` takes a `&str` — the *type information is already in hand*.

**RELEASE_READINESS_REVIEW.md:41 marks H6 "✅ Fixed"; its own evidence at :264-266 lists the exact positions still open.** The alpha.3 fix was applied per-arm rather than at the choke point its own recommendation (line 270) specified.

---

### Cluster E — Binding identity in tyc-resolve is per-scope and positional, with suppression-only carve-outs
**Explains: F4, F5, F6 (3)**

`declare_target` resolves with `lookup_local` (no parent walk). `global_nonlocal_names` is wired *only* to suppress `missing_binding_kind` — never to redirect the write, so `global CFG; CFG = ...` silently creates a phantom `Mut` local and defeats `let`/`freeze let` (F4; verified `deep_freeze` and the binding lock both destroyed in one statement). `loop_origin_spans` has no loop-body identity, so a carve-out written for *sibling* loops disables `immutable_assign` for **every** `let` in **every** loop body (F5). `from X import *` becomes a binding literally named `*` (F6) — a shape `report_unused_imports` explicitly special-cases, proving it was contemplated and never resolved.

---

### Cluster F — No single source of truth for class kind, field order, or constructor shape
**Explains: F16, F17, F18, F31, F32, F70 (6)**

Four consumers re-derive class layout from the AST, and all four infer *class kind* from incidental syntactic evidence rather than a stamped marker: `@dataclass` presence (F70), decorator name text (F31), base-name string (F17), nothing at all (F18). Multiple inheritance is left-to-right in three independent places while CPython uses reverse-MRO (F16) — producing a **three-way divergence**: checker, CPython, and VM each bind positional constructor arguments differently, all with a green build.

---

### Cluster G — The VM is a second, unverified implementation of Python semantics
**Explains: F39, F40, F41, F42, F44, F69, F70, F22, F24 + the VM halves of F6/F19/F20/F36/F37 (9–14)**

There is **no automated differential testing against CPython anywhere in CI**. The recurring micro-mechanism is a *defensive fallback that converts "cannot represent / don't know" into a plausible wrong answer*: `as i64` (F39), `args.into_iter().next()` (F40, F42), `unwrap_or(Ordering::Equal)` (F41), a missing match arm falling to `false` (F44), `return True` (F37). Three of these produce **silently wrong numbers with exit 0**.

`tyc run` currently fails on **15/15 example apps** (9 from F69's missing package context), so the VM half of the regression net has never run against the shipped corpus — which is how F70 survived.

---

### Cluster H — The generated runtime is untested string templates
**Explains: F36, F37, F38, F29 (4)**

`typhon_runtime/*.py` lives as `&str` constants in build.rs — not type-checked, not unit-tested, not in any differential harness. `_matches` has a permissive `return True` tail and **no Protocol arm and no NewType arm**, so `as!` to an `interface` hard-crashes on a *conforming* value while `as!` to a `newtype` is entirely unchecked. `parallel.py` was written against a design documented in **9 places plus the bundled skill** (`sys._is_gil_enabled()` fallback) that **does not exist in any Rust or generated Python** — measured 64× pessimisation.

---

### Cluster I — Opt-in optimisation passes change program semantics
**Explains: F25, F27, F28, F29/F38 (4)**

Gating predicates are hand-written **partial** AST visitors with `_ => {}` catch-alls (`walk_expr_purity`, analyse:3201), and eligibility conditions documented as exhaustive are not (`reductions.rs` lists 7; the post-loop-liveness and non-raising conditions are missing). alpha.5's claim that these are "opt-in ... so no previously-*correct* program changes behaviour" is false once the knob is on: `tyc build -O` on a **default** `typhon.toml` produces `NameError`, silently wrong values, and a ~420× slowdown.

---

### Cluster J — Shared behaviour lives in the CLI binary crate; the LSP re-implements or omits it
**Explains: F47, F48, F49, F50, F52, F53, F63 (7)**

`apply_strictness` is in `crates/tyc`, so the editor honours **zero** `[strictness]` severity knobs. `check_impl` omits comptime substitution that the other pipeline performs. The shape registry reads **disk**, not the live `SourceFile` inputs the backend already holds. `set_text` is unconditional and salsa 0.26's `set_field` doesn't compare, so the crate's entire raison d'être — incrementality — is defeated (180 ms/keystroke at 100 modules). And `venv_introspect.rs` is the ungated duplicate that makes **F63** a live RCE past a documented kill-switch.

---

### Cluster K — Emitter precedence/escaping incomplete + **no post-emit parse gate**
**Explains: F33, F35, F64, F30 (4)**

`expr_precedence` (printer:1827) is consulted by `BoolOp`, `BinOp`, `UnaryOp` — and by neither `Compare` nor `If`. Escapers handle interior runs but not boundary runs. The load-bearing systemic fact: **nothing re-parses the bytes the compiler writes**, and `format_source`'s `Err` — ruff telling us the output is unparseable — is discarded at build.rs:1096. `RELEASE_READINESS_REVIEW.md:114-117` asserts "**The emitter is correct**"; F33 refutes it.

---

### Cluster L — Recursion / traversal without depth or cycle guards
**Explains: F60, F61, F62 (3)**

`Checker::is_assignable` recurses forever on mutually-recursive parametric aliases (**SIGABRT on a 5-line file**, and `detect_cyclic_type_aliases` deliberately treats those edges as legal so it can never fire). The file collector follows symlinks with `Path::is_dir()` and no visited set (**infinite hang**). `infer_expr_ctx` has no depth counter; a 256 MiB stack is the only defence.

---

### Cluster M — Security & packaging boundary
**Explains: F63, F54, F55, F56, F61 (5)**

F63 is Cluster 0 #4. F55 is an ordering bug with no collision detection: the stray-`.py` copy phase runs **after** emit, so `src/X.py` silently replaces the compiled `src/X.ty` — *the program that ships is not the program that was type-checked*, and `tyc migrate` leaves exactly that state behind by default. F56: `format!(">={py}")` on a `3.13t` target emits invalid PEP 440, permanently breaking `uv sync`/`.venv`/`ruff`/introspection for **3 of 6 documented targets**.

---

### Cluster N — Docs/status drift, including docs compiled into the binary
**Explains: F65, F66, F67, F68 + 5 status corrections**

`cheatsheet.md`, `docs/diagnostics/*.md` and the skill are `include_str!`'d — the shipped `tyc` binary prints a `gather:` snippet that does not parse, a `lazy import` line that silently produces an eager import, and (via `tyc explain frozen_assign`) a `frozen class` spelling that has never parsed.

**Status corrections (report these as findings in their own right):**
| Claim | Location | Reality |
|---|---|---|
| H6 flow-narrowing ✅ Fixed | RELEASE_READINESS:41 | 1 of 5 positions fixed (F13, F15) |
| H4 `TYC_NO_INTROSPECT` covers CLI **+ LSP** ✅ | RELEASE_READINESS:27, SECURITY.md:47 | LSP half never implemented (F63) |
| "The emitter is correct" | RELEASE_READINESS:114 | F33 |
| reductions preserves exception semantics | reductions.rs:44-50 | F28 |
| `sys._is_gil_enabled()` fallback | 9 doc sites + skill | Does not exist (F29/F38) |

---

### Cluster O — CI gates cannot see any of this
**Explains: F72, F73, F74 (3)**

Source-map assertions are vacuous (F72). **25 tests** that execute emitted Python or drive the REPL silently no-op when `python3` is absent — and the CI `test` job installs no Python (F73). The perf gate spends **~81%** of its measured wall-clock in subprocesses, so a **2× regression of the entire in-process pipeline passes** (F74). `examples/` is gated on `tyc check` only; `stress/` is in no workflow and its harness asserts nothing.

---

## 2. State of the codebase — honest assessment

**Genuinely solid.** Not faint praise; these were adversarially probed and held:
- **Diagnostic catalog**: 87/87 codes have a doc page, a `tyc explain` entry, and a `url()` whose filename matches. Zero dead variants. That is rare.
- **Supply chain**: every third-party Action SHA-pinned, `deny.toml` with an **empty** ignore list, vendored fork pinned + licensed.
- **Comptime sandbox**: an allow-list interpreter with depth caps and checked arithmetic. No escape found.
- **Crash resistance**: 2,500 mutation-fuzz iterations, 0 panics. `tyc fmt` idempotent on 1,332/1,333 corpus files. 31/31 reference `.py` byte-identical to fresh builds.
- **VM control flow**: `finally` semantics, context-manager suppression, closures, dict ordering, dataclass value semantics, float repr — all byte-matched CPython.
- **Core structural lattice**: numeric widening, `bool ⊆ int`, invariance, tuple covariance, `LitStr` — correct and tested.

**Structurally fragile.** Four things, in order:
1. `preprocess.rs` as a text rewriter — it produces the only defect class the project's own VM/CPython cross-check is *architecturally incapable* of catching.
2. The two-copy assignability layer plus per-arm obligation handling inside a 30,875-line `lib.rs`.
3. The VM as an unverified parallel implementation — 15/15 apps fail, so it has never been exercised against the shipped corpus.
4. The LSP as a partial re-implementation of the CLI, sharing neither config handling, nor the check pipeline, nor the venv gate.

**The signal that should worry you most is not the count — it's the silence.** ~20 of 74 findings produce a *wrong answer with exit 0*. The project's headline correctness mechanism (VM ↔ CPython parity) cannot detect any defect originating upstream of the AST, and a large share of the silent ones live exactly there.

**Second signal: three items are marked ✅ Fixed and are not.** In two cases the *evidence text in the same document* lists what was left open. Fixes were validated against the reported repro, not against the mechanism.

### 1.0 blockers

| # | Finding | Why it blocks |
|---|---|---|
| 1 | **F10** attribute assignment unchecked | Not a bug — a missing feature at the centre of the value proposition. `self.field = v` is the mandated idiom. |
| 2 | **F63** LSP RCE past a documented kill-switch | A published security control that does not exist. SECURITY.md is currently false. |
| 3 | **F33 + no parse gate** | "Every `.ty` emits valid `.py`" is *the* promise; violated with exit 0. |
| 4 | **F1/F2/F3/F43** string-constant corruption | Silent data corruption surviving check + build + both execution surfaces. |
| 5 | **F7** `Dog?` → `Animal?` rejected | `T?` is the headline feature; broken for the headline shape. |
| 6 | **F69/F70 + no differential harness** | `tyc run` is documented as a drop-in and fails on 15/15 apps. |
| 7 | **F19/F20** `?` corrupts control flow | Silent wrong branch / infinite loop from clean check + build. |
| 8 | **F60/F61/F62** abort & hang | A compiler must not SIGABRT on 5 lines or hang on a symlink. |
| 9 | **F56** free-threaded targets | 3 of 6 documented `target` values are unusable end-to-end. |
| 10 | **F16/F17** ctor argument scrambling | Silent wrong-field writes / import-time crash from a green build. |

### Acceptable alpha debt
F65 (secret wordlist), F67/F68 (docs — cheap, do anyway), F74 (gate composition), F71 (stress fixture policy), F62 (needs 80k terms), F54 (atomicity), F51 (`List[int]`), F52 (one knob), F30/F35/F64 (narrow shapes, and the parse gate catches two of them). **F53 I would not defer** — editor/CI severity divergence actively erodes trust in the whole diagnostic surface for ~1 day of work.

---

## 3. Prioritised action plan

Sequenced so that gates precede fixes, and fixes that subsume others precede the tail.

### Tier 0 — Gates (do first; ~1 week; nothing below is verifiable without these)

| # | Change | Where | Why first | Effort | Risk | Verify |
|---|---|---|---|---|---|---|
| **T0.1** | **Post-emit parse gate.** Re-parse `python_src` with the vendored parser after formatting; fail the build. Stop discarding `format_source`'s `Err`. | `tyc/src/commands/build.rs:1096` | Catches F33, F35, F64 **and every future printer bug**. Highest yield/effort in the report. | **S** | Fails builds that "work" today — but by construction those emit unparseable Python | Corpus stays green; add the 3 repros as tests |
| **T0.2** | **Differential VM ↔ CPython harness in CI.** Every `examples/` + `stress/` file: `tyc build && python3.13` vs `tyc run`, diff stdout+exit. | new CI job | Permanently gates Cluster G (9–14 findings). Surfaces F39/40/41/42/44/69/70/22 immediately. | **M** | Starts red — record the ~37 known divergences as a baseline expectations file and burn down | The baseline file shrinks |
| **T0.3** | **Repair the gates themselves.** `actions/setup-python` @3.13 + `TYC_REQUIRE_PYTHON=1` panic (F73); one value-level sourcemap assertion, delete `ty_line >= 1` (F72); `TYC_NO_INTROSPECT=1` + `format=false` in perf-gate, re-baseline (F74). | ci.yml, build_features.rs, pipeline.rs, perf-gate.sh | Without this, Tiers 1–3 regress invisibly. 25 tests currently self-disable. | **S** | None | Remove python from PATH → suite must fail |
| **T0.4** | **Widen the net**: build+run every example under CPython; run under VM; expectations file for `stress/`; one project per opt-in knob. | pipeline.rs | Cluster I and every opt-in codegen path have **zero** coverage today. | **M** | None | New jobs green |

### Tier 1 — 1.0 blockers

| # | Change | Where | Effort | Risk / compatibility | Verify |
|---|---|---|---|---|---|
| **T1.1** | **F10 — type-check attribute assignment.** Resolve receiver via `infer_expr_readonly`, look up field in `class_shapes`, substitute generic params, `c.mismatch` on failure. Drop `narrow_attr` when rejected. | `tyc-types:10301` | **M** | ⚠️ **Highest FP risk in the plan.** Gate hard: skip `partial` shapes, absent fields, free TypeVars, `unsafe_depth > 0`, `Unknown`/`Any` receivers | 136 `self.X =` sites in `examples/` alone — **full corpus verification mandatory**; keep `impl[T]` free-TypeVar and `int→float` widening green |
| **T1.2** | **F63 — delete the LSP's duplicate introspection.** Call `tyc_venv::discover_python`; add the `allowed_top_level` gate; `.current_dir()` a scratch dir. Correct SECURITY.md + H4 status. | `tyc-lsp/venv_introspect.rs:111` | **S** | None — only removes execution | Assert no subprocess spawns with the env var set |
| **T1.3** | **Cluster A — one shared lexical mask.** Build `LexMask` (in_string / in_comment / bracket_depth / logical-line-start) once per buffer; convert the enum branch, `collect_chain`+`render_chain`, the sealed-union probe, `rename_whole_word`, the gather scan. | `tyc-syntax/preprocess.rs` | **M–L** | Pure narrowing of *when* rewrites fire → cannot reject a correct program. One deliberate widening: restores `T?` where string desync suppresses it | F1/F2/F3/F23 repros; corpus byte-identical |
| **T1.4** | **F19/F20 — lift `?` to a control-flow point.** `elif C:` → `else:` + nested `if`; `while C:` → `while True:` + prologue + `if not C': break`. **Schedule as one job with pending [20].** | preprocess.rs:5994 | **M** | Additive (only changes miscompiled programs). Interim: reject with a targeted diagnostic | Emitted branch structure; VM parity |
| **T1.5** | **F7 — Union/Union arm.** `act.all(|a| exp.any(|e| is_assignable(e,a)))` **before** the expected-Union arm. | `tyc-types:3341` | **S** | **Provably additive** — strictly relaxes | `Animal?`/`Shape?`/`Drawable?`/`int?` repros; reverse `Animal?→Dog?` still rejected |
| **T1.6** | **F33 — precedence guards.** `c.left` + every comparator vs {Lambda, If, BoolOp, Not}; `i.body`/`i.test` at `<= 2`; `i.orelse` unguarded. | `tyc-emit/printer.rs:1074, 969` | **S** | Emit-only, additive (adds parens) | T0.1 gate + round-trip test |
| **T1.7** | **F69 — VM package context.** Thread the importing module's dotted package onto the frame; strip `level-1` segments. | `tyc-vm/interp.rs:462` | **M** | Additive; also fixes the silent direction (VM succeeds where build crashes) | 9/15 apps run; add that inverse case as a test |
| **T1.8** | **F70 — VM model fields.** Key on `BaseModel` in `shape.bases` (already present) or a desugar-stamped marker. | `tyc-vm/interp.rs:865` | **S** | Additive | Example 17 runs under `tyc run` |
| **T1.9** | **F56 — derive `requires-python`** from `parse_python_target` (`>={major}.{minor}`); validate `[project] name` as PEP 508. | `deps.rs:232, 405` | **S** | Packaging-only | All 3 `t` targets sync + create `.venv` |
| **T1.10** | **F60/F61 — guards.** Visited-pair set + budget in `is_assignable`, **return `true`** on exhaustion; canonicalised visited-dir set + `symlink_metadata` in the collector. | `tyc-types:3205`, `util.rs:185` | **S** | `true` on exhaustion ⇒ cannot reject anything | 5-line alias repro; symlink matrix |
| **T1.11** | **F16/F17 — C3 linearisation + `ClassKind` marker.** One `class_layout()` shared by tyc-types / tyc-desugar / tyc-vm; `keyword_only_ctor` from `shape.bases`. | types:8921, desugar:4007, vm:932, types:9163 | **M** | Reverse-MRO is additive (fixes both the scramble **and** the mirror FP). The *interim* positional-rejection alone is **not** — must accompany, not replace | Diamond case; `dataclasses.fields()` order; VM repr parity |

### Tier 2 — High-severity, after the gates exist

| # | Change | Where | Effort | Compatibility flag |
|---|---|---|---|---|
| **T2.1** | **Cluster D choke points** — `eval_stmt_expr()` for narrowing invalidation; `assign_to_target()` covering attr paths + tuple unpack (incl. the straight-line `assign_unpacking_target` hole) | types:10940 / 9919 / 16964 | **M** | ⚠️ **`reset_global_narrowings` is call-agnostic and already false-positives today.** Naive fix propagates FPs into `if`/`while`/`for` — the highest-traffic positions. **Must** gate on a project-wide set of globals actually rebound under `global NAME` |
| **T2.2** | **F14** nullable receiver on attribute paths (`attr_path_of` + display string) | types:15975, 15302 | **S** | ⚠️ **Sequence after pending [75]** (and-chain narrowing) or it FPs on the canonical guarded idiom. Land warn-level first |
| **T2.3** | **F8** per-head READ_VIEW table in **both** copies (`Sequence`/`Reversible` ⇒ {list,tuple}; `Iterator` ⇒ {}) | types:378 **and** 3363 | **S** | Narrowing, but only on already-`TypeError` programs (alpha.2 precedent). Genexpr safety verified |
| **T2.4** | **F9** required-arity on `Type::Function`; relax both gates | types:9232, **3132** (not just 470) | **M** | Pure relaxation |
| **T2.5** | **F11** exhaustiveness gate accepts `Type::Generic` | types:11167 | **S** | ⚠️ Narrowing. The repo's own flagship linked-list example has had **zero** exhaustiveness checking. `case _:` + strictness knob are escapes; changelog it |
| **T2.6** | **F12** post-substitution invariant re-check | types:15464 | **M** | ⚠️ **Naive fix rejects two correct idioms** (int→float, Dog-into-`list[Animal]`). Must **collapse the union by subsumption first**, error only on incomparable members |
| **T2.7** | **F5** loop-body identity on `loop_origin_spans`; **F4** resolve-and-alias `global`/`nonlocal` | resolve:1052, 1736 | **M** | ⚠️ **Both narrow currently-correct programs.** F4's shape is *documented as permitted* (language.md:488). Land warn-level or behind `[strictness]`; corpus scan for F5 = 0 hits |
| **T2.8** | **F6** star imports — expand, or per-scope `has_wildcard_import` suppression — **plus the VM import path** | resolve:1530 + vm | **S** | Only removes errors |
| **T2.9** | **VM value-conversion batch**: F39 exact float→BigInt (+ Overflow/ValueError); F40 `sum` start; F41 `py_cmp_inner` bytes arm **then** harden the `Equal` fallback; F42 start/end **at the 9 call sites, not `single()`**; F44 bool/object/frozenset/complex | vm value.rs / builtins.rs | **S each** | VM-internal. ⚠️ Do **not** make `single()` reject extras — breaks `dict.get`/`pop`/`setdefault`, `str.center`/`ljust`/`rjust` |
| **T2.10** | **F22/F24** ExceptionGroup in the VM + `except*` modelling + `tyc::return_in_except_star` | vm builtins:4050, emit:679 | **M** | New diagnostic fires only on already-unparseable output |
| **T2.11** | **F36/F37** `_matches`: Protocol arm (`__protocol_attrs__`) + `__supertype__` unwrap; mirror in VM | build.rs:3143/3147 | **S** | Protocol arm strictly relaxes; NewType is alpha.2-class narrowing |
| **T2.12** | **Cluster J** — `apply_strictness` → shared crate (F53); wire `"off"` (F52); `check_impl` = with_imports (F48); feed `documents` to the registry via `uri_matches_path` (F50); `uri_to_path` at :233 (F47); guard `set_text` (F49) | tyc-lsp + tyc-db | **M** | All LSP/CLI-side. Zero language-surface risk |
| **T2.13** | **Cluster B** — `LineMap` composed through preprocess → desugar → emit → format; assert `line_offsets.len() == output.lines().count()` at write time | preprocess.rs, build.rs:1161 | **M–L** | Tooling-only. Closes F45/F46/F34 **and pending [59]** |
| **T2.14** | **Cluster I** — exhaustive `walk_expr_purity` (F25); post-loop liveness + non-raising predicate in reductions (F27/F28); `_is_gil_enabled()` + `_MIN_SIZE` in `map_pure`, free-threaded gate on the rewrite (F29/F38) + correct 9 doc sites | analyse:3201, reductions.rs:243/320, build.rs:3405/1017 | **M** | auto-memoise half is **free** (silent path); explicit `@memo` error surface is an alpha.2-class narrowing. GIL guard must sit **after** `_try_interpreters` |

### Tier 3 — Tail

F30 (annotation-nullability scoping, must match qualified `typing.Literal`) · F31 (decorator marker via resolver evidence, all 3 sites) · F35/F64 (f-string brace + tail quote-run — caught by T0.1 but fix properly) · **F55** (source/output stem collision — warn-first, since a draft `.ty` beside a working `.py` builds today) · F54 (hoist `atomic_write`) · F57/F58 (bracket-depth in migrate & fmt; stop swallowing ruff's `Err`) · F59 (migrate docstring indent) · F51 (`List[int]` normalisation + suppress the same-case-differs hint) · F65 (`PASSPHRASE` only + doc corrections) · F62 (depth guard **≥ 20,000**, warn-level) · **Docs pass**: F66/F67/F68 + the 5 status corrections (note: `include_str!` ⇒ needs a rebuild to reach users).

### Fixes to **reject** as proposed
- **F71's suggested fix** (suppress `object` at the subscript-assign site) — would make `d["k"] = v` *more permissive than* `let n: int = v`, a real soundness hole. The finding is a docs/release-notes issue, not a code one.
- **F42's** "make `single()` reject extra args" — breaks 4+ correct call sites.
- **F15's** `body_contains_call → reset_global_narrowings` — proven FP on a correct program.
- **F65's** broad wordlist (`COOKIE`/`DSN`/`SIGNING`/`CERT`) — warn-noise on `SIGNING_ALGORITHM = "RS256"`.
- **F62's** budget of 2,000 — a 3,000-term program builds and runs correctly today.
- **F16's** interim positional-rejection *as a substitute* for reverse-MRO.

---

## 4. Structural recommendations

**R1 — Kill the duplicated derivations, by name.** Work the Cluster 0 table as a checklist: unify the two assignability traversals behind one function parameterised by a nominal-knowledge callback; one `class_layout()`; one `value_matches_type_name()` in the VM; delete `tyc-lsp/venv_introspect.rs`; make `check_impl` *be* `check_source_file_with_imports`; move `apply_strictness` down a crate. Then add the rule to CLAUDE.md: **"If two crates need the same derivation, it lives in the lower crate. A second copy is a defect, not a shortcut."** `SECRET_NAME_KEYWORDS` (alpha.6) is the precedent — it was hoisted only *after* drifting twice.

**R2 — Split the monoliths along the axis that causes bugs, not by line count.** `tyc-types/src/lib.rs` (30,875) and `preprocess.rs` (12,508) are dangerous because they contain giant `match`es where cross-cutting obligations must be remembered per arm — not because they are long. The valuable extractions are `assignability.rs` (one traversal), `assignment_sites.rs` (one `check_assignment`), `narrowing.rs` (one `eval_stmt_expr` + `assign_to_target`), `class_layout.rs`, and `preprocess/lexmask.rs`. Each closes a whole cluster. **Splitting for size alone would not have prevented a single one of these 74 findings.**

**R3 — Make "two implementations" testable rather than hopeful.** VM↔CPython differential in CI is the single highest-value addition (T0.2). Apply the same idea *inside* the checker: a property test asserting `assignable(a,b) ⇒ Checker::is_assignable(a,b)` over a generated type universe catches F7 and the next drift for free.

**R4 — Two invariants the pipeline should enforce on itself:** (a) the emitted bytes re-parse; (b) `line_offsets.len() == output.lines().count()` at the moment the sourcemap is written. Add an emit round-trip property test (`parse(emit(ast)) ≡ ast`).

**R5 — Ban `_ => {}` in analysis visitors.** Make the compiler force coverage when Ruff adds a node. `walk_expr_purity` is the proximate cause of F25; audit its siblings.

**R6 — Stop inferring semantics from surface text.** Decorator name text (F31), `@dataclass` presence (F70), base-name strings (F17), `is_lazy` ignored (F66), `Protocol` without `runtime_checkable` (F36). Desugar should *stamp* explicit markers the back ends key on.

**R7 — Ban silent fallbacks in value conversion and type matching.** `as i64`, `unwrap_or(Equal)`, `args.first()`, missing-arm-⇒-`false`, `return True`. These produced 6 findings, 3 of them silent-wrong. This is a reviewable rule and largely a lintable one.

**R8 — Fix the status-tracking process.** Three ✅ Fixed items are open, two of them contradicted by evidence text in the same document. New rule: **a fix closes only when the *mechanism* is closed and a regression test exercises the mechanism**, and every "Fixed" row cites a test name. Separately, alpha.2's changelog bundled "3 conservative new diagnostics" (which carry the additive promise) with a "newly-typed positions" batch (which does not) — future release notes must separate the two.

**R9 — CI the docs that ship inside the binary.** `cheatsheet.md`, `docs/diagnostics/*.md` and the skill are `include_str!`'d. Extract and `tyc check` every code block (the harness needs a fragment wrapper — cheatsheet.md uses 4-space indentation, not fences).

**R10 — Adopt an explicit compatibility taxonomy.** Findings split three ways and the project currently conflates them: **(a) pure relaxation** (F7, F9, T2.8) — always safe; **(b) narrowing on already-crashing code** (F8, F17, F36) — the alpha.2 carve-out, needs a changelog line; **(c) narrowing on correct-running code** (F4, F5, F11, F12, F18, F26, T2.1) — needs a warn-level release, a `[strictness]` knob, or a docs change *first*. Tag every fix with its class in the PR template.

---

## Coverage critique

*Produced by a dedicated critic agent whose only job was to find what the review missed.*

## COVERAGE CRITIQUE — 26-agent Typhon review, 2026-07-28

### A. Ground truth: what the tree actually contains

Measured with `wc -l` over `/home/user/Typhon/tyc/crates` (138,257 first-party Rust lines) plus `find tyc/vendor -name '*.rs'` (53,337 vendored lines). Production-vs-test split measured by the first `#[cfg(test)]` in each file:

| file | total | production | tests |
|---|---|---|---|
| tyc-types/src/lib.rs | 30,875 | **18,173** | 12,702 |
| tyc-syntax/src/preprocess.rs | 12,508 | 8,818 | 3,690 |
| tyc-lsp/src/lib.rs | 6,317 | **3,951** | 2,366 |
| tyc-desugar/src/lib.rs | 5,981 | **4,311** | 1,670 |
| tyc-lsp/src/semantic.rs | 2,275 | 1,185 | 1,090 |
| tyc-venv/src/lib.rs | 2,359 | 1,602 | 757 |
| tyc-analyse/src/extend_builtin.rs | 869 | 692 | 177 |

Two coverage claims are inflated by this split: the LSP reviewer's *"Read all 6317 lines of `crates/tyc-lsp/src/lib.rs`"* and the desugar reviewer's *"Read the whole of `tyc-desugar/src/lib.rs` (5,981 lines)"* both count each crate's own test module as source read. Real production surface read is ~63% and ~72% of the stated figure. Not dishonest, but it means "whole file" claims across this review should be discounted ~25%.

---

### B. Files NO agent read (first-party production code)

Cross-referencing every `crates/...` path cited in the 26 coverage notes against the real tree, these files are cited by **zero** reviewers:

| file | prod lines | why it matters |
|---|---|---|
| `tyc-lsp/src/semantic.rs` | 1,185 | semantic tokens + the LSP's **own** signature parser (`parse_signature`, `CalleeSignatures`) |
| `tyc-analyse/src/extend_builtin.rs` | 692 | the v0.15.5 headline feature; touches checker + codegen + VM |
| `tyc/src/commands/repl.rs` | 636 | "skimmed only, no repro run" — and the tyc-db reviewer's FP finding lands *here* |
| `tyc-lsp/src/stdlib_stubs.rs` | 625 | seeds stdlib shapes into every LSP session |
| `tyc-vm/src/ffi.rs` | 390 | "read only cursorily; no finding" — the VM's escape hatch to real CPython |
| `tyc/src/commands/init.rs` | 369 | generates every new project's `typhon.toml` |
| `tyc-syntax/src/lexer.rs` | 258 | explicitly declined by the syntax reviewer |
| `tyc-emit/src/stub.rs` | 257 | `.pyi` emission — explicitly declined |
| `tyc/src/commands/profile.rs` / `install.rs` | 513 | feed `pgo-memoise`; ship the bundled skill |
| `tyc-vm/src/error.rs`, `tyc-syntax/src/ruff.rs`, `tyc/src/cli.rs` | 342 | — |
| **`tyc/vendor/**` (Ruff fork)** | **53,337** | nobody, in either audit; UPSTREAM pin `astral-sh/ruff @ 3fcc9823` currency unverified |

**~5,700 lines of never-read first-party production Rust, plus the entire 53k-line vendored parser.**

### C. The `tyc-venv` hole is total, and it is the worst one

The `tyc-venv` domain summary is the literal string `Test.` for both summary and coverage — **that agent produced nothing**. I confirmed the gap is not filled elsewhere:
- The security reviewer read only `discover_python`, `which_python3`, `INTROSPECT_SCRIPT` and the allow-list — and then spent its finding on `tyc-lsp/src/venv_introspect.rs`, the *other* implementation.
- `grep -n "venv\|introspect" docs/adversarial-audit-50agent-2026-06-28.md` returns **zero hits** — the prior 50-agent audit never covered it either.
- Nobody exercised introspection against a real installed third-party package (only the classes reviewer installed pydantic, for a different purpose).

So the crate that (a) spawns a Python subprocess, (b) executes `import` of arbitrary user-named modules, (c) parses free-text annotation strings via a hand-rolled recursive descent (`annotation_to_type` at `crates/tyc-venv/src/lib.rs:756`, `split_top_level_pipes:971`, `split_generic:895`, `union_from_members:1007`), and (d) feeds the result straight into `tyc::type_mismatch` — has **never been reviewed by anyone, ever**. Every one of those parsers is a false-positive generator by construction: a mis-parsed annotation becomes a wrong `Type` and then a hard error on correct user code, which is this project's stated worst-case defect class.

### D. Defect classes structurally invisible to this review's design

1. **Regression / additive-compatibility violations.** 25 of 26 agents probed only the *current* binary. The one agent that ran an old baseline (corpus health, re-running `stress/round-2026-06-21/harness.sh`) immediately found a shipped alpha.2 additive-compatibility violation. A 1-for-1 hit rate on the only method that can detect the project's #1 hard constraint, used by 4% of the review.
2. **Cross-module shape propagation.** All four `tyc-types` reviewers independently listed cross-module as NOT covered; tyc-db explicitly declined `build_external_shapes` (~330 lines). Yet v0.14.1, v0.15.4, v0.15.5 and alpha.4's H5 fix are *entirely* cross-module work. Highest change-rate subsystem in the compiler, zero exercise.
3. **Pass interaction.** Every agent stayed inside one crate. No file combining `as!` + `gather:` + `rescue` + `impl` on a sealed union was ever compiled — yet the surviving findings show `preprocess.rs` runs these as sequential, mutually-blind text passes over one buffer.
4. **Opt-in codegen knobs in combination.** `[optimise] level=1` + `traceback-remap` + `auto-gather` + free-threaded: the corpus reviewer confirmed zero gated coverage and probed four in isolation only.
5. **`.dty` stubs.** `find . -name '*.dty'` returns exactly two files, both compiler-bundled. **Zero `.dty` in `examples/` or `stress/`**, and no agent took the surface. I verified it is live and load-bearing: a scratch `src/redislib.dty` type-checks call sites and emits `build/redislib.pyi`, with no `.py` and no diagnostic that the module will `ImportError` at runtime.
6. **Non-Linux, concurrency, real free-threaded 3.13t** — declined by every agent that touched them, for environmental reasons. Unavoidable here, but it means the entire `parallel-backend`/free-threaded wave in alpha.5 shipped and has now been reviewed twice with zero execution.

### E. Suspicious distribution

- `tyc-venv`: 1,602 production lines → **0 findings, 0 reading**.
- `tyc-lsp`: 4 surviving findings, all from `lib.rs`; `semantic.rs` (1,185 prod lines, 23% of the crate) black-box smoke-tested only. The LSP's own surviving finding is *"line-count-changing expansions shift every position below them"* — and `semantic.rs:189 compute_with_original` takes `line_shifts: &[usize]` which, per its own doc at line 186, records only *"the byte count stripped from the start of that line"*. It has no line-count mechanism at all. The identical bug is almost certainly live in the token path and nobody looked.
- `tyc-types`: 4 agents, and the union of their cited line ranges covers roughly 5,500 of 18,173 production lines (~30%). The uncited block `17,415–18,173` contains the entire `tyc::incompatible_override` checker (`check_override_compatibility` at line 17,451), `check_enum_match_exhaustiveness:17702`, `check_pattern_class_fields:17778`, `collect_inherited_fields:17918` and `bind_pattern_names:18000` — a whole diagnostic family plus the pattern-binding core, unreviewed. (I read `check_override_compatibility`'s variance arms to check for an inverted `is_assignable`; they are correct — `is_assignable(expected, actual)` per line 3045, so `is_assignable(sub_p, base_p)` is genuine contravariance and `is_assignable(base_ret, sub_ret)` genuine covariance. Not a defect; noted so the next agent does not re-derive it.)

---

### F. Ranked follow-up investigations

**1. Full `tyc-venv` review (zero prior coverage, two audits running).**
First step: read `crates/tyc-venv/src/lib.rs:756-1066` (`annotation_to_type`, `split_generic`, `split_top_level_commas`, `split_top_level_pipes`, `union_from_members`) and build a table-driven probe of hostile annotation strings — forward refs (`"Foo"`), `Literal["a,b"]`, `Callable[[int, str], None]`, `dict[str, list[tuple[int, ...]]]`, `Annotated[int, Field(gt=0)]`, PEP 604 nested unions — asserting each maps to `Type::Unknown` rather than a *wrong* concrete type. Any wrong concrete type is a shipped false positive.

**2. Cross-module shape propagation, exercised end-to-end.**
First step: build a 4-module project where module A declares a generic `class Box[T]` + sealed union + newtype + frozen class, B re-exports via `pub *`, C consumes through `import` and through `from … import`, D through a qualified `a.Box[int]` annotation — then flip one shape in A and confirm C/D re-check. Targets `build_external_shapes` and the alpha.4 H5 guard, which four reviewers each declined.

**3. `tyc-lsp/src/semantic.rs` position mapping under line-expanding preprocessing.**
First step: drive `textDocument/semanticTokens/full` over a file with a `gather:` block at line 10 and an identifier at line 40, and assert the returned token line equals 40. `compute_with_original` (line 189) consumes only per-line *column* shifts; the LSP's surviving line-shift finding predicts this returns 40+N. Concrete, cheap, and would confirm a second instance of an already-confirmed mechanism in unread code.

**4. Cross-release differential regression harness.**
First step: check out the `v1.0.0-alpha.4` tag, build it, then run both binaries' `tyc check` over all 1,342 `examples/` + `stress/` `.ty` files and diff the diagnostic sets. Any file that alpha.4 accepted and alpha.6 rejects is an additive-compatibility violation. This is the only method in the whole review that found one, and it was used once.

**5. `tyc-analyse/src/extend_builtin.rs` (692 unread lines) beyond the logged `import mod` case.**
First step: read `extend_builtin.rs` in full, then probe receiver positions the logged item [46] does not cover — extension method on a comprehension target, on a `dict[str,str]` subscript result, on a narrowed `str?`, chained (`t.shout().shout()`), and inside an `unsafe:` block. I confirmed the plain `import mod` + bare-name receiver case reproduces (matches logged [46]), so the pass is demonstrably fragile; the question is how much of the registry is wrong vs. just the lookup site.

**6. `.dty` stub surface, first-class review + corpus fixtures.**
First step: read `tyc-emit/src/stub.rs` (257 lines, unread) and write a `.dty` declaring a generic class, a sealed union, a `Result`-returning method and an `async def`, then diff the emitted `.pyi` against the declaration. Also determine whether a `.dty` with no runtime module should produce a diagnostic — my scratch build at `/tmp/claude-0/-home-user-Typhon/21c6b520-9bcf-5d26-ae4b-afec08b3b0ed/scratchpad/dt` emitted `build/redislib.pyi` with no `.py` and exit 0, so the program `ImportError`s at run time from a green build.

**7. Multi-feature interaction fuzzing of `preprocess.rs`.**
First step: write a generator that emits files combining 2–4 of {`gather:`, `with`-chain, `as!`, postfix `rescue`, `?`, `|>`, `enum` body, triple-quoted string, `impl` on a sealed union} at randomised nesting, and assert `tyc check` clean ⇒ `tyc build` output is byte-stable and `tyc run` agrees with CPython. Every surviving syntax finding is a single-pass bug; the passes are sequential and mutually blind, so the composition space is untested.

**8. Opt-in knob matrix over the real corpus.**
First step: for each of the 16 `examples/apps/*`, run `tyc build` under the cross product of `[optimise] level ∈ {0,1}` × `traceback-remap ∈ {false,true}` × `auto-gather ∈ {false,true}`, execute each artifact under python3.13, and diff stdout against the `level=0` baseline. Any divergence is an opt-in path changing a correct program's behaviour — the exact claim alpha.5's release note makes.

**9. Vendored Ruff fork currency + delta audit.**
First step: `git -C tyc/vendor` has no history; instead diff each vendored crate against upstream `astral-sh/ruff@3fcc9823` (per `tyc/vendor/UPSTREAM`) to enumerate the Typhon delta, and check upstream's advisory/CVE history since that commit. 53,337 lines with a never-verified pin, in the component that turns untrusted text into an AST.

**10. `tyc repl` and `tyc profile` execution paths.**
First step: drive `tyc repl` over stdin with the exact program from the tyc-db reviewer's `check_impl` comptime-substitution finding to confirm the FP surfaces there (that finding is code-grounded on `check_impl` but the REPL path itself was never run), then fuzz REPL stdin for panics — `repl.rs` is 636 unread lines that compile arbitrary input through the full pipeline.

---

## Appendix A — Method

26 domain reviewers, grouped so related features were reviewed together: pipeline stages
(syntax/preprocess, resolve, desugar, emit+sourcemaps, generated runtime); the type system split
by concern (core lattice, generics/variance/HKT, flow narrowing, classes/dunders,
Result/`?`/rescue); analysis (async/`gather:`/`go`, comptime/purity, perf+parallelisation); the VM
split by size (core semantics, builtins/stdlib parity); tooling (LSP, Salsa DB, venv, diagnostics,
build/config, migrate/fmt/trace); and cross-cutting sweeps (robustness/panics, security, docs
drift, corpus health, tests/CI).

Every reviewer was required to invoke the bundled `typhon` skill first, to cite
`crate/file.rs:line` with a causal mechanism, and to paste real executed command output — with an
explicit instruction that a fabricated repro is worse than no finding, and that returning zero
findings is a respectable result. Each was given the 44 still-pending items from the June 2026
audit as a do-not-re-report list.

Verifiers were prompted to refute, to default to REFUTED when unable to reproduce, to check for
duplicates against the logged audits, and to sanity-check both the claimed severity and whether
the suggested fix would violate the project's additive-compatibility constraint.

## Appendix B — Unverified leads

Reported by a domain reviewer but **never independently verified** — ranked 4th or lower within
their domain. Severity is reviewer-assigned and uncalibrated. Treat as a backlog of hypotheses,
not as findings.

| Domain | Sev | Category | Title |
|---|---|---|---|
| analyse-async | HIGH | vm-parity | A failing `go` background task aborts the whole program under the VM (exit 1) but is logged-and-ignored under compiled CPython (exit 0) |
| analyse-async | MEDIUM | soundness | `auto-gather` folds an await-run that sits inside a `try:` block, silently converting a caught exception into an uncaught ExceptionGroup — a correct program breaks on opting in / `tyc build -O` |
| analyse-async | MEDIUM | diagnostics | A gather binding whose call spans multiple lines fails to lower and surfaces as a misleading `tyc::parse` error on the `gather:` header itself |
| analyse-comptime | HIGH | emit-runtime-crash | `@memo` hashability check is blind to Typhon's own `class` form, so `@functools.cache` is injected over a `@dataclass(slots=True)` parameter and the emitted Python dies with `TypeError: unhashable type` |
| analyse-comptime | MEDIUM | soundness | `@pure` never checks method-call callees, so a pure function may mutate module-level state via `MODULE_LIST.append(x)` — the exact case its own doc comment claims to forbid |
| analyse-comptime | MEDIUM | diagnostics | A comptime float that is infinite or NaN renders as the unparseable literal `inf.0` / `NaN.0`, and the silent string-literal fallback turns it into a nonsensical `tyc::type_mismatch: expected float, found str` |
| analyse-perf | MEDIUM | diagnostics | tyc::perf_sort_in_loop gives result-changing advice: its invariance check ignores attribute writes to the collection's elements, which are exactly what a repeated sort responds to |
| cli-build | LOW | performance | Source collection follows directory symlinks with no visited-set guard, so a symlink inside src/ multiplies compilation and explodes the output tree to 40 levels deep |
| cli-build | MEDIUM | architecture | tyc build never prunes stale artifacts and there is no tyc clean; after renaming the entry module, tyc run --compile silently executes the deleted previous program |
| cli-build | MEDIUM | docs-drift | tyc build --check is documented as a dry-run that does not touch disk, but it writes pyproject.toml, creates .venv and uv.lock, and runs a network-capable uv sync |
| cli-migrate | HIGH | architecture | tyc migrate silently overwrites an existing hand-edited .ty file with no backup, prompt, or --force gate |
| cli-migrate | MEDIUM | test-gap | tyc check --stubs ignores parameter defaults and async-ness, so a .dty promising a default the implementation lacks passes clean and the built program dies with TypeError |
| corpus | HIGH | test-gap | The regression net gates only per-file `tyc check` on `examples/`: no build, no execution, no VM, and `stress/`'s 1083 files are not wired into CI at all |
| corpus | MEDIUM | test-gap | Coverage-gap map: headline v1.0.0-alpha features and every codegen-affecting optimisation knob have zero representation in the gated corpus |
| db-salsa | LOW | docs-drift | `check_file` / `check_file_with_imports` allocate a brand-new Salsa input on every call, so the tracked `check_diagnostics` memo can never be reused and each call permanently grows the database — the documented "instant cache hits" claim is false |
| db-salsa | MEDIUM | architecture | Salsa is not wrapped behind a Typhon-owned module — `tyc-db`'s public API exposes `&dyn salsa::Database` and the macro-generated `set_text`, forcing a direct `salsa` dependency on `tyc-lsp` and violating the project's stated top meta-rule |
| desugar | HIGH | emit-runtime-crash | rewrite_mutable_field_defaults rewrites `ClassVar` fields to `dataclasses.field(default_factory=...)`, making the emitted module raise TypeError at import |
| desugar | HIGH | soundness | synthesise_raw_class_init folds a mutable literal field default into the generated `__init__` parameter default, giving every `class!` / exception-subclass instance the SAME list/dict/set |
| desugar | MEDIUM | vm-parity | TypedDict-literal→constructor rewrite fires on `plain class`, which has no synthesised `__init__` — clean check, `TypeError` at runtime, and the VM disagrees |
| diagnostics | LOW | diagnostics | Five diagnostics the compiler files as warnings carry no `severity(Warning)` attribute and render with the red error glyph `×` |
| diagnostics | LOW | diagnostics | `tyc::main_not_called` help text prints a literal `\n` escape instead of a line break |
| docs-drift | LOW | docs-drift | docs/configuration.md — the canonical `typhon.toml` reference — omits three shipping `[strictness]` keys, and guide 10 states that a wired knob (`stub-check`) is unimplemented |
| docs-drift | MEDIUM | diagnostics | `tyc::lazy_usage` help text and its doc page recommend two forms that don't work — a non-existent `lazy val` keyword, and a `lazy import` of a class that type-checks clean then crashes at runtime |
| emit-sourcemap | HIGH | soundness | Multi-line string literals are written without recording their interior newlines, desyncing the `.py.map` table for the rest of the file |
| emit-sourcemap | MEDIUM | emit-runtime-crash | Overflowing/NaN float literals emit bare `inf` / `-inf` / `nan`, producing a NameError at import from a clean `tyc check` and `tyc build` |
| lsp | MEDIUM | diagnostics | Cross-file go-to-definition lands `len("pub ")` columns before every exported symbol — `map_preprocessed_offset_to_original` has an inverted keyword table |
| resolve | HIGH | vm-parity | No definite-assignment model for function locals: a read before the local's first assignment resolves cleanly, and the VM answers with the module global where CPython raises UnboundLocalError |
| resolve | MEDIUM | emit-runtime-crash | An `except … as e` name stays bound after its handler, so a post-handler read type-checks clean and crashes at runtime (CPython deletes the name) |
| resolve | MEDIUM | emit-runtime-crash | Import vetting collapses every project module to its root segment, so a nonexistent submodule of an existing first-party package is never diagnosed and dies with ModuleNotFoundError |
| robustness | MEDIUM | vm-parity | VM silently truncates out-of-range sequence repetition counts to zero (returns '' / [] where CPython raises OverflowError), and aborts the process on large-but-representable counts where CPython raises a catchable MemoryError |
| runtime | HIGH | vm-parity | `_LazyValue` still omits `__format__`, `__setitem__`, `__delitem__` and `__setattr__` after the audit-[23] operator-forwarding fix — f-string formatting and in-place mutation of a module-level `lazy let` crash under CPython but work under the VM |
| runtime | LOW | diagnostics | A user module named `typhon_runtime` is silently shadowed by the generated package — clean `tyc check`, successful build, ImportError at runtime, with no shadow diagnostic |
| runtime | MEDIUM | performance | `typhon_runtime/__init__.py` eagerly imports `tasks` and `parallel`, dragging `asyncio` + `concurrent.futures` into every program that merely uses `Result` — ~36 ms of unavoidable startup cost |
| syntax-preprocess | MEDIUM | diagnostics | `tyc check` reports mislocated line numbers and leaks synthetic `__typhon_*` source for any file containing a `gather:` block (trigger not covered by the logged `?`/`as!` case) |
| syntax-preprocess | MEDIUM | performance | `as!` / `rescue` fixpoint lowering is O(k·n): each occurrence rebuilds the whole buffer and re-scans from byte 0 with a fresh full-file skip mask |
| tests-ci | MEDIUM | architecture | A directly-pushed `v*` tag publishes release binaries with zero tests executed, and no CI job ever runs on macOS or Windows despite shipping binaries for both |
| tests-ci | MEDIUM | test-gap | No automated VM↔CPython differential test exists; VM parity is asserted against hand-authored expectations inside the .ty fixture, never against the reference implementation |
| types-classes | HIGH | soundness | `find_method`/`find_field` resolve multiple-inheritance members in reverse base order (LIFO DFS), so the last base wins instead of Python's first-base MRO precedence |
| types-classes | HIGH | soundness | Every `ClassVar[T]` read is typed `Unknown` — the declared type is discarded, so class constants flow into any annotation unchecked |
| types-core | HIGH | false-positive | Method-call arity is looked up by bare attribute name in the module-level function table — a module (or imported) function named `get`/`keys`/`values`/`items` makes every `dict.get(...)` fail with `tyc::missing_argument` |
| types-core | LOW | false-positive | `complex` is absent from the numeric tower — `let z: complex = 1` and `let w: complex = 2.5` are rejected |
| types-flow | HIGH | soundness | Attribute narrowing is invalidated only by a bare method-call *statement* — the same call in an assignment RHS leaves `self.x` narrowed |
| types-flow | MEDIUM | soundness | `finally` body is checked with the try-body's end-state narrowings, though `finally` also runs on paths that exited the body early |
| types-flow | MEDIUM | soundness | A nested function that rebinds a captured local via `nonlocal` does not invalidate the enclosing function's narrowing of that local |
| types-generics | HIGH | false-positive | User-generic variance inference (and HKT constructor-variable recovery) scan only the `class` body, never `impl`/`extend` blocks — so the idiomatic Typhon form is always Invariant and produces a `tyc::type_mismatch` false positive |
| types-generics | MEDIUM | status-correction | The documented `@covariant` / `@contravariant` variance override is unusable — the resolver rejects the decorator name with `tyc::unknown_name`, making `explicit_variance_override` dead code |
| types-result | HIGH | vm-parity | `tyc-vm` is the only pipeline that omits `expand_inline_question_ops`, so every non-statement-tail `?` is a hard parse error under `tyc run` (the default execution mode) |
| types-result | HIGH | soundness | `rescue` block whose mapper is `str(e)` — the form used by the shipped `examples/60-rescue-boundaries` — escapes the `result_error_mismatch` check, because a builtin-constructor call infers as `Unknown` |
| types-result | MEDIUM | emit-runtime-crash | A module-level `rescue` block emits `return Err(...)` outside any function — clean `tyc check`, exit-0 `tyc build`, CPython SyntaxError, while the VM runs the module fine |
| vm-core | HIGH | vm-parity | In-place augmented assignment on a set (\|=, &=, -=, ^=) rebinds instead of mutating, so aliases and identity diverge from CPython |
| vm-core | MEDIUM | vm-parity | Value::py_cmp has no arms for bytes or set/frozenset, so `b"a" < b"b"` and `{1} < {1,2}` raise TypeError under `tyc run` on valid Python |
| vm-core | MEDIUM | vm-parity | str.swapcase() truncates multi-character Unicode case mappings (drops characters) and str.casefold() is only lowercase, not case folding |
| vm-stdlib | HIGH | vm-parity | VM sum() drops its positional start argument and uses naive float accumulation instead of CPython 3.12+ compensated summation, returning materially wrong numbers |
| vm-stdlib | HIGH | vm-parity | VM enumerate(iterable, start) silently ignores a positional start, so the ubiquitous 1-based line-numbering idiom is off by one |
| vm-stdlib | MEDIUM | vm-parity | VM json.dumps ignores ensure_ascii (CPython default True) and separators, so serialised JSON bytes differ from the compiled build for any non-ASCII payload |
