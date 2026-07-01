# Changelog

All notable changes to Typhon are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) loosely; the
canonical phase-by-phase status lives in `docs/roadmap.md`.

## Unreleased — release-readiness remediation

A full-codebase release-readiness review (`RELEASE_READINESS_REVIEW.md`) and the
fixes it drove. The positive corpus (`examples/`) stays clean and the `stress/`
negative-fixture counts are unchanged, so no previously-correct program changes
behaviour.

### Licensing / packaging

- **Added the missing licenses.** A repository-root MIT `LICENSE`, the upstream
  Ruff MIT notice at `tyc/vendor/LICENSE` (© 2022 Charlie Marsh), and a copy under
  `editors/vscode/`. Release archives now pack both `LICENSE` and `LICENSE-ruff`
  unconditionally (a missing file fails the release instead of being skipped).
- **Community/security scaffolding.** New `SECURITY.md` (with the trust model),
  `CONTRIBUTING.md`, and `.github/dependabot.yml` (github-actions / cargo / npm).
- **Release hygiene.** Alpha/pre-release tags are now published with
  `prerelease: true` (derived from a `-` in the tag) instead of carrying the
  "Latest" badge, and auto-tag only fires after CI succeeds on the commit
  (`workflow_run` gate) so an untested merge can't auto-publish binaries.

### Compiler / VM fixes

- **Type checker: nested-generic assignability is no longer exponential.** The
  invariant-container check used `assignable(a,b) && assignable(b,a)`, doubling
  the recursion at every level (O(2^depth)); a deeply-nested annotation like
  `list[list[…list[int]]]` could hang `tyc check` / the LSP. A single-pass
  `types_equivalent` with identical semantics makes it linear (depth-28 check:
  5.4 s → 7 ms).
- **VM: cyclic values no longer abort the process.** `Value::py_eq` / `py_cmp`
  recursed without bound on self-referential containers, overflowing the native
  stack (`a=[]; a.append(a); b=[]; b.append(b); a == b`). They are now
  depth-guarded with an `Rc::ptr_eq` identity fast-path.
- **VM: `str.find` / `rfind` / `index` / `rindex` return character offsets**, not
  byte offsets — so `s[s.find(x):]` is correct on non-ASCII text.
- **VM: augmented assignment on a list mutates in place** (`b = a; b += [x]` and
  `self.items += [x]` now match CPython's `list.__iadd__`).
- **VM: float `%` follows the divisor's sign**, and float `//` / `%` by `0.0`
  raise `ZeroDivisionError` instead of returning `inf` / `nan`.
- **VM: `print` tolerates a broken pipe** (clean exit on `tyc run app | head`
  instead of a Rust panic).
- **VM: `json.dumps` coerces scalar dict keys to strings** (valid JSON;
  `{1: "a"}` → `{"1": "a"}`).

### Tooling / LSP

- **LSP reserves a 256 MiB stack** (matching the CLI), so deeply-nested input
  can't overflow the tokio blocking-pool stack and kill the language server.
- **`tyc fmt` writes atomically** (temp file + rename), so a crash mid-write can
  no longer truncate a source file.
- **`TYC_NO_INTROSPECT`** disables venv dependency introspection in the CLI and
  LSP — a kill-switch for the "opening a project imports its dependencies" trust
  boundary (documented in `SECURITY.md`).
- **Windows venv discovery** now probes `.venv\Scripts\python.exe` and
  `python` / `py`, not just the Unix `.venv/bin/python` and `python3`.
- **`[strictness] exhaustive-match`** is now actually applied — `"warn"` demotes
  `tyc::non_exhaustive_match` to a warning and `"off"` drops it (previously the
  validated knob silently did nothing).

### Diagnostics / docs

- **Diagnostic doc URLs** point at resolvable GitHub docs paths instead of the
  never-deployed `typhon.dev`.
- **Four diagnostics gained doc pages + `tyc explain` entries**
  (`empty_collection_no_annotation`, `freeze_not_freezable`,
  `newtype_invalid_base`, `typing_alias_in_annotation`), completing the catalog.
- **Docs corrections:** stale "current release" pointers (`docs/install.md`,
  `docs/long-term-plan.md`), the README quickstart (`cd myapp`), the docs-site
  install page (pre-built-binary path + correct Rust floor), the emit config page
  (`class-default = "pydantic"` is rejected, not accepted), examples index
  (`60-rescue-boundaries`), and the VS Code grammar (dropped the non-existent
  `val` / `var` / `Option` / `Some` keywords; added an Install section).

## 1.0.0-alpha.2 — 2026-06-29 — type-checker soundness sweep + VM parity

The remediation of the [2026-06-28 adversarial pre-release review](docs/adversarial-review-2026-06-28.md):
a sweep of type-checker **soundness** holes, **false positives** on idiomatic
code, **VM ↔ CPython** parity gaps, and parser / config / crash robustness — plus
three new conservative diagnostics (`tyc::not_a_context_manager`,
`tyc::raise_non_exception`, `tyc::frozen_inheritance_conflict`). The headline
soundness fixes close two silent-wrong clusters on everyday code: flow narrowing
of a **non-local** place (global / instance field) is now invalidated across an
intervening call or alias write, and short-circuit narrowing (`x is not None and
x.method()`) no longer false-positives on the canonical Python null-check idiom.
**No new syntax, and no previously-*correct* program changes behaviour.** Unlike
the purely-additive releases before it, this one does narrow the accepted surface
in one direction: the three new diagnostics reject programs that already crashed
at runtime — `raise 42` / raising a plain dataclass, an invalid local `with`
subject, and mixed frozen/non-frozen dataclass inheritance type-checked clean
before but raised at import/run, and now surface as build-time errors instead. A
program that type-checked clean *and ran correctly* on `v1.0.0-alpha` is
unaffected; if you have code that relied on one of those runtime-crashing shapes,
expect a new (correct) diagnostic. The full positive corpus (`examples/` +
`examples/apps/`) type-checks clean and emits runnable Python (the `stress/`
tree includes deliberately-negative cases and is not a clean-build gate);
`cargo test --workspace`, `cargo clippy -D warnings`, and `cargo fmt --check` all
pass. As an alpha, the surface syntax remains *not yet frozen*.

### Added

- **`tyc::not_a_context_manager`** — a `with` / `async with` whose subject is a
  local class lacking the protocol (`__enter__`/`__exit__`, or
  `__aenter__`/`__aexit__`) is rejected at check time instead of crashing.
  Stdlib/third-party CMs and `@contextmanager` factories stay permissive.

### Fixed

- **Calling an instance with a user `__call__` is typed as that method's
  return type**, not the class — closing a soundness hole (`let r: Adder =
  a(5)` accepted) and a false positive (the correct `let r: int = a(5)`
  rejected).
- **Nested `def` references are checked against a declared `Callable`.** A
  closure returned/assigned where a `Callable[[int], str]` was promised is now
  rejected when its real signature differs (was typed `Unknown`).

### Docs

- Clarified the `as!` checked-cast contract: `Callable` signatures,
  user-defined generic classes, and abstract collection types
  (`Sequence[X]`, …) verify only the erased origin at runtime (generics are
  erased in CPython); the VM enforces the same recursive check as the build
  path (no longer an identity pass-through).

- **`match` or-pattern captures are typed as the union of alternatives.**
  `case A(n) | B(n)` over `A | B` bound `n` from the first arm only; it now
  yields `A.x | B.x`, so a value matching a later arm can't be used at the
  wrong type.
- **`list[T] += rhs` checks the RHS element type** (`xs: list[int]; xs +=
  ["bad"]` is rejected), and **dict-comprehension key/value expressions are
  checked** against the annotated `dict[K, V]`.

- **Tuple-unpack targets are typed.** `for k, v in d.items()` and
  `let a, b = pair()` bound each slot to `Unknown`; they now destructure the
  element `tuple[K, V]`, so passing a `str` key into an `int` parameter is
  caught instead of crashing at runtime.
- **`match` star/double-star captures are typed.** `case [a, *rest]` binds
  `rest` to `list[T]` and `case {"k": v, **rest}` binds `rest` to `dict[K, V]`
  (were `Unknown`), so `rest.upper()` is now rejected.
- **Slice reads are typed as their container** (`list[T][a:b]` → `list[T]`,
  `str[a:b]` → `str`), and **subscript assignments are checked** against the
  element/value type (`data[0] = "x"` / `data[0:1] = ["x"]` into a `list[int]`,
  `d[k] = "x"` into a `dict[str, int]` are rejected). All were silent
  corruptions before.

- **`raise <non-exception>` is rejected at check time** (new
  `tyc::raise_non_exception`). `raise 42` and `raise Problem(...)` (a plain
  dataclass) type-checked clean and then crashed with CPython's
  `TypeError: exceptions must derive from BaseException`. The check is
  conservative — only literals and locally-defined classes with a fully-known
  non-exception ancestry fire; builtin/imported/venv-introspected exceptions
  (e.g. `fastapi.HTTPException`) and `Exception` subclasses stay permissive.
- **Parameter default values are type-checked against their annotation.**
  `def f(n: int = None)` / `= "z"` crashed at runtime; now rejected, while
  `int? = None`, `int | None = None`, and `float = 0` stay valid.
- **`ClassVar` fields are excluded from the constructor signature.** Passing
  one as a kwarg (`Config(DEFAULT_PORT=…)`) crashed at runtime; it's now
  rejected, and the field stays accessible as a class attribute.
- **Over-deep relative imports are flagged correctly** (off-by-one fix): a
  depth-0 `from .x` and a depth-2 `from ...x` reach above the package root and
  crash the emitted Python; both are now caught while in-bounds `from .sibling`
  still passes.

- **`bytes %`-formatting (PEP 461) is accepted and runs.** `b"%d items" % 5`
  and `b"%d-%s" % (5, b"x")` were rejected at check time as
  `operator_type_mismatch`; the `%` carve-out now covers `bytes` (result
  `bytes`) like `str`, and the VM implements `bytes.__mod__` so `tyc run`,
  compiled CPython, and reference CPython all agree.
- **`isinstance()` now narrows an attribute target.** `if isinstance(b.v, int):
  return b.v` was wrongly rejected — only bare names narrowed. Attribute paths
  now narrow through the same machinery as `is None`, which already resets on
  reassignment.
- **Exhaustive `match` with an or-pattern over a literal union / bool no longer
  fires a spurious `missing_return`.** `case "red" | "green":` /
  `case True | False:` are recognised as covering their alternatives.
- **Bare `Final` / `ClassVar` annotations (PEP 591) infer their type from the
  value** instead of failing with `type_mismatch`.
- **`frozen` / non-`frozen` dataclass inheritance is rejected at check time**
  (new `tyc::frozen_inheritance_conflict`). The combination type-checked clean
  but the emitted module crashed on import with CPython's `TypeError: cannot
  inherit frozen dataclass from a non-frozen one` (both directions). Only
  in-module dataclass bases are compared, so external/non-dataclass bases are
  unaffected.
- **Module-level `lazy let` of a primitive is transparent under all
  operators.** The emitted `_LazyValue` proxy only forwarded attribute access,
  so `VALUE + 1`, `VALUE > 10`, `range(VALUE)`, `NAME + " world"` crashed the
  compiled program (while the VM ran). The proxy now forwards arithmetic,
  reflected, comparison, bitwise, unary, conversion, index and membership
  dunders; laziness is preserved.
- **`await` inside a `gather:` binding (and `go await …`) no longer emits
  crashing CPython.** `a = await fa()` lowered to `create_task(await fa())`,
  raising `TypeError: a coroutine was expected`; the redundant leading `await`
  is now stripped before wrapping.
- **`class!` subclass of an in-module field-bearing base constructs
  correctly.** Its synthesised `__init__` accepted only its own fields and
  opened with a no-arg `super().__init__()` that hit the base's field-requiring
  constructor and raised `TypeError`; the constructor now accepts and assigns
  the inherited fields directly.
- **Emitted `.pyi` stubs keep the `@dataclass` decorator** so consumers'
  type-checkers synthesise `__init__` and accept correct keyword construction
  (previously stripped, which made every stubbed dataclass reject
  `Cls(field=…)`).
- **`typhon.toml` rejects unknown keys and invalid `[checker] external`.** A
  typo'd section/key (`[pyhton]`, `taget`) silently reverted to defaults (e.g.
  a default-3.13 build); config structs now `deny_unknown_fields`, and
  `[checker] external` is validated against `none`/`ty`.
- **`tyc` no longer aborts (SIGABRT) on deeply-nested or very long
  expressions.** A long flat `a + b + c + …` chain (plausible generated code)
  or deeply nested brackets built a deep AST that overflowed the default
  ~8 MB stack during the recursive type-check / VM walk. The CLI now runs on a
  256 MiB worker stack (lazily committed), pushing the ceiling far past any
  realistic program.
- **Multi-line `freeze let` with a `#` inside a string no longer fails to
  parse.** A continuation line like `"list": ["a#b"],` made the bracket-depth
  scanner treat the in-string `#` as a comment and miss the following `]`, so
  the synthesised `__typhon_freeze__(` was left unterminated and `tyc check` /
  `run` / `fmt` all rejected a valid program. The scanner now tracks single-
  and triple-quoted string state.
- **`tyc explain` now resolves every shipped diagnostic.**
  `field_default_ordering`, `use_of_uninitialised`, `pub_name_collision`,
  `missing_field_init`, `pub_star_outside_init`, and `kind_mismatch` (the last
  with a new doc page) were emitted by the compiler and documented but returned
  "unknown diagnostic code"; they're now registered in the catalog and
  `tyc explain --list`.
- **VM dict/set keys collapse numerically-equal `int` / `float` / `bool`**, matching
  CPython (`{1: a, 1.0: b, True: c}` is one entry; `1 in {1.0}`; `hash(1) ==
  hash(1.0)`; `frozenset({1, 2.0}) == frozenset({1.0, 2})`). The `HashKey`
  equality, hashing, and canonical ordering now treat an integral float like the
  equal int; non-integral floats keep their own identity. Closes a silent VM/
  CPython data divergence.
- **VM now enforces the `as!` checked cast** (was an identity passthrough). The
  in-process VM interprets the `as!` type descriptor and runs the same recursive
  structural check as the compiled path's `typhon_runtime/cast.py`, so a
  wrong-shaped value raises `TypeError` under `tyc run` exactly as it does under
  `tyc build && python` — closing a VM/CPython divergence on the boundary-cast
  enforcement path (`tyc run` is the default execution mode). Scalars (with
  `int→float` / `bool→int` widening), `list`/`set`/`frozenset`/`dict`/`tuple`
  (fixed and variadic), `Optional`/unions, and user classes are all checked;
  anything unmodellable stays permissive.
- **Type checker: flow narrowing of a module global is invalidated by an
  intervening call.** `if g is not None: clear(); use(g)` (where `clear()`
  reassigns the global via `global g`) no longer keeps the stale non-`None`
  narrowing — a silent soundness hole that checked clean and raised at runtime.
  Local-variable narrowing is unchanged (a call can't rebind a caller's local).
- **Type checker: short-circuit narrowing reaches later bool-op operands.**
  `x is not None and x.method()` (and the De Morgan `x is None or x.method()`)
  no longer false-positive with `tyc::nullable_use` on the method receiver — the
  canonical Python null-check idiom now type-checks. The narrowing is contained
  to the expression (it doesn't leak to later uses, and a walrus binding an
  operand introduces is preserved).

## 1.0.0-alpha — 2026-06-22 — first feature-complete alpha

**Typhon's first tagged alpha.** This release rolls up milestones **M1**
("Tidy & deepen") and **M2** ("Type-system frontier") of the
[alpha release plan](docs/alpha-release-plan.md), plus the early M3 items
(formatter idempotence, the performance-regression CI gate) and the `rescue`
exception-boundary sugar. It is **additive on the accepted surface** since
v0.15.7 — every previously-accepted program type-checks identically.

**Compatibility & stability statement.** The production path
(`tyc build` → CPython 3.13+) is stable: emitted Python is valid, idiomatic,
and carries **no runtime dependency** on the Typhon toolchain (only a small
generated `typhon_runtime/` package when you use `Result` / `go` / `freeze let`
/ `lazy let` / auto-parallel / `as!`). The full `examples/` + `examples/apps/`
+ `stress/round-2026-06-21/` corpus builds to runnable Python and checks clean.
As an **alpha**, the *surface syntax is not yet frozen* — it may still change
before `1.0.0` with a documented migration note (per the plan's explicit
non-goals). There is no semver stability guarantee on the language surface yet.

**Known limitations deferred to beta** (tracked in
[`TYPE_SYSTEM_FRONTIER.md`](TYPE_SYSTEM_FRONTIER.md) and the alpha plan's M3 tail):

- **Embedded `ty` (Phase 2)** is not shipped — the Phase 1 subprocess path
  (`[checker] external = "ty"` / `--with-ty`) ships and is the supported way to
  get typeshed-backed checking. Phase 2 is perf-only (blocked on vendoring a git
  source past `cargo deny`).
- **Typeshed-backed checking for pure-extension libraries** (numpy/pandas
  `.pyi`-only public APIs) is not yet automatic; use an authored `.dty` stub or
  the `ty` subprocess pass.
- **`tyc migrate` hardening** on the full curated PyPI set (B2) is ongoing; the
  `mut else:` defect (B1) is fixed.
- **Function-level HKT params** (`def f[F[_]]`), non-class constructor
  application, and constructor composition remain deferred (class-level HKT
  unification ships).

### Alpha prep — milestone M1 ("Tidy & deepen") of the [alpha release plan](docs/alpha-release-plan.md)

First batch toward the feature-complete `v1.0.0-alpha`. See
`docs/alpha-release-plan.md` for the full plan, milestones, and release gates.

- **Added — `Annotated[T, …]` is type-checked through third-party introspection
  (A1).** `tyc-venv`'s annotation mapper had no `Annotated` arm, so dependencies
  typed as `Annotated[str, FieldInfo(...)]` (FastAPI / Typer / Pydantic) degraded
  to `Unknown` and wrong-typed kwargs slipped past the checker. The mapper now
  strips the optional `typing` / `typing_extensions` qualifier and resolves the
  first type argument (metadata `repr`s with commas handled), degrading to
  `Unknown` only when the inner type is unresolvable. Relaxing/strengthening
  only — no new false positives on the corpus.
- **Added — `tyc::stdlib_module_shadow` now fires on `tyc build` (B3).** The
  diagnostic existed but was `tyc check`-only; a top-level `types.ty` / `ast.ty`
  / `parser.ty` that silently shadows a stdlib module is now warned about when
  building too (non-fatal). `parser` and `this` added to the curated stdlib list.
- **Added — cross-file go-to-definition for import aliases and `.py` siblings
  (E1).** `tyc lsp` go-to-definition now resolves `import foo as f` then `f.Bar`
  to `Bar`'s definition in `foo`, splices dotted receivers (`pkg.sub.Thing`),
  and resolves `.py` / `__init__.py` siblings (with `.ty` winning). A missing
  named member now falls back to the local declaration site instead of jumping
  to the wrong file top.
- **Fixed — `tyc migrate` no longer emits invalid `mut else:` / `let else:`
  (B1).** Keyword-led compound headers (`else:`, `try:`, `finally:`, …) were
  misclassified as annotated-assignment targets and prefixed with a binding
  marker. The migrate pass now skips Python reserved words, so only genuine
  (re)assignments get `let` / `mut`.
- **Docs — `roadmap.md` refreshed to v0.15.7** (per-release summaries through the
  0.14.x / 0.15.x line); `examples/apps/TYPHON_FEEDBACK.md` banner-stamped as
  historical (captured at v0.5.2 — most findings since resolved). CI gate audit
  confirmed `fmt` / `clippy -D warnings` / `test --workspace` / corpus
  round-trip are all already enforced.

### Alpha prep — milestone M2 ("Type-system frontier")

- **Added — higher-kinded type unification (C1).** `Type::TypeConstructor` is now
  wired through the unifier. A class type parameter used as a generic head
  (`fa: F[A]`, `-> F[B]`) is recognised as a constructor variable; binding a
  formal `F[A]` against a concrete `list[int]` binds `F → list, A → int` and
  substitutes consistently in the return type, so `class Functor[F[_]]` /
  `map` type-checks over builtins (`list`) and user generics (`Box[T]`). Wrong
  arity (`F[A, B]` vs `list[int]`) and conflicting constructor binding (`F` to
  both `list` and `set` in one call) now emit the new `tyc::kind_mismatch`
  diagnostic instead of silently producing `Unknown`. Function-level HKT params
  (`def f[F[_]]`), non-class constructor application, constructor composition,
  and cross-module HKT arity propagation are deferred and degrade to prior
  behavior (no new false positives).
- **Added — variance inference for user-declared generics (C2).** Each user
  generic class's type parameters are now classified by usage: appearing only
  in output positions (method return types, read-only `@property`) infers
  covariant; only in input positions (method params, settable fields) infers
  contravariant; appearing in both, behind a mutable container/field, or in any
  unprovable position stays `Invariant` (the safe default). A covariant
  `Producer[Dog]` now flows into a `Producer[Animal]` slot while an invariant
  `Box[Dog]` still does not; the builtin variance table is unchanged. A bare
  `@covariant` / `@contravariant` class decorator overrides inference.
  Cross-module variance (carrying inferred variance through `ModuleShapes`) is
  deferred and stays at the invariant default — sound, never unsound widening.
- **Fixed — sound variance through generic interface bounds (C4).** A generic
  interface bound (`def f[X: Producer[Animal]]`) checked the implementer with
  the interface's type parameter left unbound (treated as `Any`), so the type
  argument was discarded and every class spuriously conformed — a soundness
  hole that accepted covariant-return and contravariant-parameter violations.
  Conformance now substitutes the interface's type arguments into each member's
  return / parameter / field types before the assignability check, so returns
  flow covariantly and parameters contravariantly. Non-generic interface bounds
  were already correct; un-introspected third-party generic interfaces degrade
  to the prior permissive check — no false positives. A sound tightening.
- **Added — small non-nullable unions are type-checked through introspection
  (A2).** The tyc-types AST path and `is_assignable` already handled N-ary
  `Type::Union`; the leak was the `tyc-venv` string parser, which degraded any
  non-`None` pipe union to `Unknown`. It now emits a real 2-member union
  (`split_top_level_pipes` for true arity, plus a `Union[A, B, …]` branch) with
  soundness guards: `X | None` reduces to the nullable form, a redundant member
  collapses, exactly two distinct concrete members become a real union, and any
  `Unknown`/`Any` member — or 3+ members, or `X | Y | None` — degrades the whole
  union back to `Unknown`. So `Union[str, bytes]` (jinja2) and
  `Union[str, os.PathLike]` (Flask) now reject an `int` argument while per-member
  numeric widening (`int → float`, `bool → int`) is preserved. 3+ member unions
  are deferred. Zero corpus false positives.
- **Added — inter-procedural field-init audit (C3).** The `tyc::missing_field_init`
  audit (which catches `X.__new__(X)` partial instances escaping with required
  fields unassigned) now tracks partial instances across helper chains via a
  sound, dependency-ordered, cycle-safe per-function summary, replacing the two
  trivial-factory special cases. A `let c = make()` whose callee returns a
  partially-initialised instance fires at the caller's escape, across silent
  passthrough (`return X.__new__(X)`) and multi-hop chains (`make2 → make1`).
  Every uncertainty drops to not-partial and never invents a missing field:
  `setattr`, `obj.method(...)`, passing the instance into any call, any compound
  control-flow, rebinds to unknown values, and arg-bearing call shapes; the
  `unsafe:` posture is unchanged. Cross-module helper chains, branch-merge/SSA
  unification, and parameter-based partial tracking remain deferred (sound).
  Zero corpus false positives.
- **Added — cross-module variance + HKT propagation (C1/C2 cross-module).** The
  deferred cross-module halves of HKT and variance inference now flow through the
  existing `ModuleShapes` / `ExternalShapes` mechanism: a generic class imported
  from a sibling module carries its inferred type-parameter variance and its HKT
  constructor-variable identity, so `user_generic_param_variance` and the HKT arm
  consult imported generics exactly as local ones. An imported covariant
  `Producer[Dog]` now widens into a `Producer[Animal]` slot (previously stuck at
  the invariant default across module boundaries), invariant imports still
  reject, and `Functor[F[_]]`-shape HKT binds across the boundary. Variance is a
  pure relaxation (missing entry → invariant default, never a false positive);
  HKT degrades to pre-HKT permissive when an imported class's identity is
  unavailable.

- **Added — performance regression CI gate (F2).** `scripts/perf-gate.sh` times
  the full build pipeline over a real multi-module example, takes the median of
  9 runs (after 2 warmups), and fails CI when it exceeds the committed
  `perf-baseline.json` by >20%. Methodology and re-baselining are documented in
  `docs/performance-baseline.md`. Designed to be non-flaky (median, warmup,
  generous threshold) and dependency-light (bash + python3 + jq, no network).
- **Fixed — `impl Alias:`-over-sealed-union diagnostics point at real source
  (B15).** Distributing one `impl Alias:` block into one block per variant
  byte-duplicates the method body, appending later blocks past EOF of the real
  file, so a diagnostic inside a duplicated body rendered a line number beyond
  the file's length (the length-preserving `sanitize_synthetic_source` restored
  the header text but kept the duplicated lines — its `TODO(#32)`). A new
  `BlockRemap` at the existing diagnostic-sanitisation chokepoint detects the
  signature (a run of 2+ same-indent `impl <Name>:` blocks with byte-identical
  bodies) and remaps each label's span per-line back onto the first
  real-source-aligned block (per-line deltas keep columns correct across uneven
  variant-name lengths). Body diagnostics now resolve to the exact authored
  member line; no diagnostic can exceed the real line count. Files without a
  distributed impl group are untouched. Closes the long-standing B15 limitation
  with no invasive source-map plumbing.

- **Fixed — `tyc fmt` is now idempotent and semantics-preserving (E2).** Two
  source-corrupting bugs are fixed: `extend BUILTIN:` (e.g. `extend str:`) was
  rewritten to the internal lowering `extend class
  __typhon_builtin_ext_str(object):`, and generic `impl[T] Name[T]:` / generic
  `extend` headers were mangled to `impl Name:` across two passes. Both headers
  are now restored verbatim (line-map-aware) while their bodies are still
  formatted, so `tyc fmt --check` is a clean no-op across the whole corpus and
  formatting never changes program meaning. Bracket-depth-aware `:` / `->` / `=`
  spacing confirmed (annotation/dict spaced, slice colons tight, kwarg/default
  `=` tight, statement `=` spaced). The B15 synthetic-line-number leakage is
  deferred — a correct fix needs a source-span remap threaded through
  `tyc-syntax` / `tyc-resolve` / `tyc-types`, outside the formatter's scope.

### Added — `rescue`: lambda-free, `try`/`except`-free exception boundaries

Two new forms — a postfix operator and a block prefix — lift a throwing boundary
into a `Result` with no lambdas, no `try`, and no `except` in source.

**Postfix** (`EXPR rescue NAME: ERR_EXPR`) catches an exception from a single
expression, maps it, and propagates the `Err` to the enclosing `Result`-returning
function like `?`:

```python
def load_port(raw: str) -> Result[int, str]:
    let n: int = int(raw) rescue e: f"bad port: {e}"
    return Ok(n)
```

**Block** (`rescue NAME: ERR_EXPR:` over a suite) maps any exception raised
anywhere in the suite — the apples-to-apples replacement for a `try/except` shim:

```python
def load_config(text: str) -> Result[Config, str]:
    rescue e: f"bad config: {e}":
        let data: dict[str, str] = json.loads(text) as! dict[str, str]
        return Ok(Config(host=data["host"], port=int(data["port"])))
```

Both are pure surface sugar (in `tyc-syntax`, folded into `expand_question_ops`
so they reach every pipeline including the VM): the postfix form lowers to
`try_result(lambda: EXPR, lambda NAME: ERR_EXPR)?`; the block form lowers to a
`try` / `except Exception as NAME: return Err(ERR_EXPR)`. They work identically
under `tyc check`, `tyc run` (VM), and `tyc build` + CPython, compose with `as!`,
and `tyc fmt` round-trips them. The block form runs to a fixpoint, so nested
`rescue` blocks expand.

This replaces the hand-written `try: return Ok(…) except E as e: return Err(…)`
shim and the lambda-heavy `try_result(lambda: …, lambda e: …)` call. See
`docs/design/error-boundary-sugar.md` and `docs/guides/06-error-handling.md`.

The mapped error type **is** checked against the enclosing function's declared
error type: a postfix `rescue`/`try_result` whose mapper type doesn't match fires
`tyc::result_error_mismatch` (see the f-string fix below, which closed the last
hole here), and a block `rescue` emits a real `Err(...)` so the existing
`return Err(...)` error-type check covers it.

**Scope (v1):** postfix `rescue` lowers in **statement-tail** position (the last
thing on the logical line — the `…)?` shape every pipeline's end-of-line `?` pass
handles); an inline/mid-expression postfix `rescue`, or one whose right side
isn't `NAME: EXPR`, is left for the parser.

### Fixed — f-strings now infer as `str` (closing a type-checking hole)

`infer_expr_ctx` had no `Expr::FString` arm, so every f-string fell through to
`Unknown`, silently disabling type-checking of f-string values: `let x: int =
f"{n}"` was accepted, and a `try_result`/`rescue` error mapper written as
`lambda e: f"bad: {e}"` inferred `Result[T, Unknown]`, so a mismatched error type
slipped past the `?` propagation check. F-strings now infer as `str` (and their
interpolated expressions are walked so their own diagnostics surface), so those
cases are caught. Verified against the full `examples/` + `examples/apps/` corpus
with zero regressions.

## 0.15.7 — 2026-06-21 — third-party introspection depth + stress-round robustness

A robustness release that deepens compile-time checking of third-party code and
clears a fresh batch of type-checker false positives surfaced by the 2026-06-21
stress round (~130-program breadth sweep) and a wide third-party introspection
audit (43 libraries). The headline is that **third-party *method* calls are now
arity-checked** — a missing required argument to `PCA(...).fit()` or
`df.merge()` is caught at `tyc check` / `tyc build` time, the same way
constructor and free-function calls already were. Three introspection-robustness
fixes (a proxy member that raises from `inspect.signature` no longer silently
disables a whole module's checks; the implicit-Optional `x: T = None` idiom no
longer false-positives; `pkg.sub.Thing()` multi-segment attribute calls are now
checked like the `from`-import form) plus three pure type-checker false-positive
fixes (`Counter +/- Counter`, a `plain class` / `class!` with a hand-written
`__init__`, and a `__call__`-bearing instance passed where a `Callable` is
expected) remove build-blockers on valid, idiomatic code. A VM correctness fix
(`str.isupper()` / `str.islower()` on uncased strings) closes a silent-wrong-output
gap under `tyc run`. Rounding out the release: three compiler-perf passes (fewer
heap allocations in type narrowing, VM `repr`, source-map generation, and Path
handling), a hardened hardcoded-secret lint (now catches embedded secret terms),
and a docs-site styling refresh. All changes are backward-compatible —
previously-accepted programs type-check identically.

### Added — third-party **method** calls are now arity-checked (missing required arguments caught)

- **`tyc-venv` now introspects the public methods of every third-party class,
  not just its constructor**, so a missing required argument to a *method* call
  is caught at `tyc check` / `tyc build` time — the same way constructor and
  free-function calls already were. The motivating case: a typed ML pipeline
  where `PCA(n_components=2).fit()` (sklearn `fit(self, X, y=None)`) silently
  compiled despite the missing `X`. It now fails with
  `tyc::missing_argument: supply ``X`` when calling ``fit```, and likewise for
  `scaler.transform()` (→ `X`), `df.merge()` (→ `right`), etc.
  - The introspection script (`inspect.getattr_static` + `inspect.signature`)
    captures instance methods, `@classmethod`s, and `@staticmethod`s, stripping
    the implicit `self` / `cls` only when it is a genuine leading positional
    (a decorator-wrapped `(*args, **kwargs)` method, common in scikit-learn, is
    left fully permissive rather than having its `*args` mis-stripped).
  - Each captured method becomes a `MethodSig` on the class's `InterfaceShape`,
    so the existing `method_arity_info_for_attribute` path arity-checks the
    call. Methods whose signature can't be recovered (C-extension slots, e.g.
    most `numpy.ndarray` methods) are simply absent and stay lenient — the
    shape remains `partial`, so no false `attribute_not_found` / arity errors.
  - **Conservative by design / verified false-positive-free:** a realistic
    numpy + pandas + scikit-learn pipeline (PCA, StandardScaler, KMeans,
    LogisticRegression, RandomForestClassifier, `train_test_split`,
    `accuracy_score`, DataFrame `groupby`/`merge`/`sort_values`, ndarray
    `reshape`/`sum`/`astype`, …) type-checks clean and the emitted Python runs
    against the real libraries; the missing-argument cases above all fail with
    a named parameter. Requires the dependency installed in the project `.venv`
    (the dist→import-name resolution — `scikit-learn` → `sklearn` — already
    works via `.dist-info` metadata).

### Fixed — third-party introspection no longer silently disabled by a proxy member that raises from `inspect.signature`

- **A single module-level object that raised a non-`(TypeError|ValueError)`
  exception from `inspect.signature` (or `callable()`) crashed the introspection
  of the *entire* module**, silently skipping every third-party check for that
  library — the worst failure mode for this feature, since a skipped check looks
  identical to a clean pass. The canonical trigger is a re-exported proxy: Flask
  exposes werkzeug `LocalProxy` objects (`current_app`, `g`, `request`,
  `session`) at module scope; `callable(proxy)` is `True`, so the script probed
  them as functions and `inspect.signature` raised
  `RuntimeError: Working outside of application context`. Django's
  `django.conf.settings` (`LazySettings`) trips the same path via `callable()`.
  As a result `flask.Flask()` (missing the required `import_name`), a wrong-typed
  arg, and unknown kwargs all compiled clean.
  - **`tyc-venv`** (`INTROSPECT_SCRIPT`): `params_of` / `returns_of` now catch any
    `Exception` from `inspect.signature` (treated as "no signature recoverable" →
    the member is skipped, stays lenient), and `introspect_one` wraps each
    member's shape computation in `try/except BaseException` so one pathological
    member can only lose itself, never the whole module. Strictly widens
    robustness — recovered shapes are the same `inspect`-derived,
    conservatively-modelled ones already trusted for jinja2/sklearn, so the
    change can only *add* true positives. Regression test
    `introspection_survives_a_member_that_raises_on_signature`.

### Fixed — the implicit-Optional idiom (`x: T = None`) no longer false-positives a third-party argument

- **A third-party parameter annotated as a bare scalar but defaulted to
  `None` — the ubiquitous "implicit Optional" idiom `def f(x: int = None)` /
  `Cls(x: str = None)` — was type-checked against the *non-nullable* scalar,
  so passing `None` (or any nullable value) failed with
  `tyc::type_mismatch: expected ``str``, found ``None```.** This is a real
  false positive (a build-blocker on valid code), observed on real installed
  libraries: `redis.exceptions.AskError` / `MovedError` / `ClusterDownError`
  / `MasterDownError` (`status_code: str = None`) and `pydantic.v1.confloat`
  / `conlist` / `parse_file_as` / `parse_raw_as`.
  - **`tyc-venv`**: `INTROSPECT_SCRIPT` now captures `default_is_none` per
    parameter, and the new `param_type_from` helper widens a None-default
    param's concrete type to nullable (`str → str | None`). Only concrete,
    non-nullable types are widened — an already-`Optional[X]` annotation,
    `Unknown`, or `None` is left untouched. The widening only ever *adds*
    accepted values, so it can only remove false positives, never introduce
    one; a genuinely wrong-typed argument (`status_code=123`) still fails
    against `str | None`. Regression test
    `implicit_optional_default_none_widens_param_to_nullable`.

### Fixed — multi-segment attribute calls (`pkg.sub.Thing()`) now arity/type-checked like the `from`-import form

- **A nested-module attribute call — `sklearn.pipeline.Pipeline()`,
  `django.conf.Settings()`, `dateutil.parser.parse()`, `rich.console.Console(...)`,
  `starlette.applications.Starlette(...)` — silently skipped the
  arity/type/unknown-kwarg check** that `from pkg.sub import Thing; Thing()` and a
  single-segment `jinja2.Template()` already got. `import pkg.sub` binds the
  *top* name `pkg` (Python semantics), but venv enrichment only registers the
  introspected *leaf* `pkg.sub`, so `pkg` never became a `Type::Module` and the
  whole chain degraded to `Unknown`.
  - **`tyc-types`**: the `BindingKind::Import` arm now treats `pkg` as a module
    when it is a registry key *or the parent of one*; the `Type::Module` arm of
    both `infer_attribute` and the side-effect-free `infer_expr_readonly` (used
    to compute the call-site function name) now chain a nested submodule
    attribute to `Type::Module("pkg.sub")` instead of returning `Unknown`.
    Regression test `nested_submodule_attribute_constructor_arity_checks`.
- Found by the 2026-06-21 wide third-party audit
  (`stress/round-2026-06-21/third-party-wide/`, 43 libraries): pre-fix the
  corpus missed 7/16 must_fail cases **and false-positive-rejected a valid
  must_pass** (`redis.exceptions.AskError(status_code=None)`); post-fix it
  catches 16/16 and is clean across **44 idiomatic must_pass programs (0 false
  positives)**.

### Fixed — `Counter + Counter` / `Counter - Counter` no longer false-fire `tyc::operator_type_mismatch`

- **`collections.Counter` multiset addition (`+`) and subtraction (`-`) were
  rejected** (`tyc-types`, `tyc::operator_type_mismatch`: "unsupported operand
  types for `+`: `Counter[str]` and `Counter[str]`"), **blocking the build**,
  even though `Counter` overloads both (and the set-style `&` / `|` already
  passed via the permissive bitwise arm). `operator_operands_compatible` now
  accepts `Counter + Counter` (alongside `list`/`tuple`) and `Counter - Counter`
  (alongside the set-difference carve-out). `Counter + int` (and other
  cross-type mixes) still correctly errors. Found by the 2026-06-21 stress round
  (`stress/round-2026-06-21/`, repro `117`).

### Fixed — `plain class` / `class!` with a hand-written `__init__` no longer false-fires `tyc::unknown_kwarg`

- **A `plain class` (or `class!`) carrying a hand-written `__init__` whose
  parameter names differ from the declared fields was rejected at the
  constructor call site** (`tyc-types`, `tyc::unknown_kwarg`). e.g.
  `plain class Box: _data: dict[...]` with `def __init__(self, data): self._data = data`
  constructed as `Box(data={...})` reported "unknown keyword argument 'data'
  (did you mean `_data`?)" and **blocked the build**, even though the custom
  constructor accepts `data`. The constructor **arity** check already exempted
  `plain class` / `class!` (they may carry an `__init__` not reflected in the
  fields), but the adjacent **unknown-kwarg** loop did not. It now applies the
  same exemption. A normal `class` / `model` / `frozen` (whose constructor is
  auto-generated from its fields) still reports a misspelled kwarg. Found by the
  2026-06-21 stress round (`stress/round-2026-06-21/`, repro `106`).

### Fixed — a `__call__`-bearing instance now satisfies a `Callable` parameter

- **An instance of a class defining `__call__` was rejected where a
  `Callable[...]` was expected** (`tyc-types`, `tyc::type_mismatch`). Passing a
  callable instance — `apply(fn=my_multiplier, x=5)` where
  `my_multiplier.__call__(self, x: int) -> int` — is a standard Python pattern
  (typeshed treats such an instance as structurally callable), but the nominal
  assignability check reported `expected (int) -> int, found Multiplier` and
  **blocked the build** on code that runs correctly. `Checker::is_assignable`
  now, when the expected type is a function type and the actual is a class,
  looks up `__call__` in the class hierarchy, rebuilds its signature as a
  function type (its stored `param_types` / `return_type` already exclude
  `self`), and re-runs the contravariant-param / covariant-return function
  check. A class *without* `__call__` is still correctly rejected, so the
  change only relaxes assignability and cannot introduce a false positive.
  Found by the 2026-06-21 stress round (`stress/round-2026-06-21/`, repro `95`).

### Fixed — VM `str.isupper()` / `str.islower()` on uncased strings

- **The tree-walking VM (`tyc run`) returned `True` from `str.isupper()` /
  `str.islower()` for strings with no cased characters** (`tyc-vm`). The
  predicates were computed as "non-empty and no lowercase/uppercase char", so
  `",".isupper()`, `" ".isupper()`, `"5".isupper()`, and `",".islower()` all
  returned `True` where CPython returns `False` (the predicate requires *at
  least one cased character*). This produced silently wrong output under
  `tyc run` — e.g. a Caesar cipher that branches on `c.isupper()` / `c.islower()`
  mangled punctuation and spaces. The compiled path (`tyc build` → CPython) was
  always correct. Both predicates now require a cased character of the matching
  case and none of the opposite case. Found by the 2026-06-21 stress round
  (`stress/round-2026-06-21/`, repro `29`).

### Changed — compiler performance: fewer heap allocations on hot paths

- **`tyc-types` exhaustiveness / narrowing** (`cases_cover_type`): now compares
  type names by `&str` reference instead of allocating an owned `String` per
  type-check match, removing a dynamic `.to_string()` per case in match
  exhaustiveness and `isinstance` narrowing (#210).
- **`tyc-vm` Python `repr`** (`python_repr_str` / `python_repr_bytes`): replaces
  `write!` / `format!` (`std::fmt`) with direct string/byte assembly in these
  tight loops; **source-map generation** (`build_source_map_v2`) stringifies
  line/column integers through an `itoa` buffer instead of `format!` (#213).
- **Path handling across the compiler** (`tyc`, `tyc-vm`, `tyc-lsp`,
  `tyc-format`): `display().to_string()` is replaced with
  `to_string_lossy().into_owned()` for `std::path::Path`, dropping the
  `std::fmt` formatting machinery on a frequently-hit path (#215).
- All three are internal refactors with no behavioural change.

### Changed — the hardcoded-secret lint catches *embedded* secret terms

- **`tyc-analyse` / `tyc build`** secret-literal detection (`is_secret_name` /
  `secret_suffix`) now searches for secret terms *anywhere* within an
  identifier rather than only as a suffix, with word-boundary checks so it
  flags `API_KEY_FOO` and `myTokenValue` while still ignoring false positives
  like `MONKEY` and `PASSPORT`. New unit tests cover the camelCase and embedded
  cases (#205).

### Changed — docs-site styling refresh

- The Starlight docs theme gains elevated markdown blockquotes (subtle
  background, thicker accent border, rounded corners) and adds tab-hover and
  table focus-within styling for clearer visual hierarchy and keyboard
  navigation (#204, #212). Docs-site only — no compiler or language change.

## 0.15.6 — 2026-06-16 — stress-test robustness sweep

An ~198-program adversarial sweep ("if you can write it in Python, you can use
Typhon") spanning the breadth of what production Python apps do. Several
type-checker false positives (several of them build-blockers) and a cluster of
VM parity gaps were fixed; the compiled output (`tyc build` → CPython 3.13) is
now correct on the entire corpus. The largest cluster was around custom
exception classes (`class FooError(Exception):` + `raise FooError("msg")`).
Two small additive features also landed: flow-sensitive attribute narrowing
(`if self.x is None: return …`) and a VM `abc` module shim. All changes are
backward-compatible — previously-accepted programs type-check identically.

### Fixed — nested class patterns no longer block the build

- **An exhaustive `match` with a nested class pattern no longer false-fires
  `tyc::missing_return` (`tyc-types`).** A case like
  `case Circle(center=Point(x=cx, y=cy), radius=r):` (where `center: Point`)
  was treated as refutable because its nested `Point(...)` sub-pattern wasn't
  a bare capture, so the checker demanded a fall-through arm and **blocked the
  build** on valid, idiomatic code. A sub-pattern is now recognised as total
  when it's a nested class pattern against a field of that exact class and the
  nested pattern itself totally covers that class. A nested *value* filter
  (`x=0`) stays correctly refutable, so genuine fall-throughs still fire.

### Fixed — list `match` with a non-tail star is recognised exhaustive

- **`case [first, *middle, last]:` no longer leaves a length-coverage gap.**
  The list-length exhaustiveness check only accepted a *tail* star
  (`[a, *rest]`); a star in the middle or head (`[first, *mid, last]`,
  `[*init, last]`) was ignored, so an otherwise-complete match
  (`[]` / `[x]` / `[x, y]` / `[first, *mid, last]`) false-fired
  `tyc::missing_return`. A single star anywhere with capture/wildcard
  non-star elements now contributes its `≥ len-1` length coverage; a genuine
  gap (e.g. `[]` + `[a, *m, b]` leaving length 1) still fires.

### Fixed — `isinstance`-narrowed container is usable parametrically

- **`if isinstance(x, dict): use(x)`** where `use` wants `dict[str, object]`
  (and the `list` / `set` / `frozenset` / `tuple` analogues) no longer
  false-fires `tyc::type_mismatch`. Python's `isinstance` can't take a
  parametric type, so a container narrowed this way has unconstrained element
  types; a bare container is now assignable to its parametric form (matching
  mypy's `dict[Any, Any]`). A genuine parametric mismatch
  (`dict[str, int]` → `dict[str, str]`) still fires.

### Fixed — iterator-protocol classes conform to `Iterator[T]`

- **`def __iter__(self) -> Iterator[int]: return self`** (the standard
  custom-iterator shape, with a `__next__(self) -> int`) no longer false-fires
  `tyc::type_mismatch` on `return self`. `is_assignable` now recognises a class
  implementing `__next__(self) -> T` (or `__iter__(self) -> Iterator[T]`) as
  structurally conforming to `Iterator[T]` / `Iterable[T]` / `Collection[T]` /
  `Reversible[T]`. A class without those methods still fails.

### Added — flow-sensitive attribute narrowing

- **`if self.value is None: return …` now narrows `self.value` to non-`None`
  for the rest of the block** (and the `is not None` / `if/return` forms),
  matching how local variables already narrow. Previously every optional
  *attribute* access stayed `T?`, so the ubiquitous "check an optional field,
  then use it" pattern false-fired `tyc::nullable_use` /
  `operator_type_mismatch` / `type_mismatch` and blocked the build — even
  though both mypy and pyright accept it. Narrowing is keyed by access path
  (`self.value`, `cfg.db.host`), snapshot/restored around branches (so it
  never leaks past a non-diverging `if`), invalidated when the path is
  reassigned, and reset at each function boundary. An un-narrowed nullable
  attribute still fires.

### Fixed — `match` on a narrowed nullable subject is recognised exhaustive

- **`match s:` after `if s is None: return …` no longer false-fires
  `tyc::missing_return` (production blocker).** The exhaustiveness pass keyed
  off the *declared* subject type (`Shape?`), so it thought `None` was an
  uncovered case even though the earlier guard had narrowed `s` to `Shape`.
  `match_subject_type` now uses the *narrowed* type (falling back to declared),
  so flow-sensitive narrowing before the match is respected. An un-narrowed
  nullable subject still fires (None genuinely uncovered).

### Fixed — exhaustive tuple-of-union `match` no longer blocks the build

- **`match (state, event):` over a `tuple[Union, T]` no longer false-fires
  `tyc::missing_return` (production blocker).** The idiomatic state-machine
  dispatch where each sealed-union variant is paired with a capture/wildcard
  for the other column is exhaustive, but the checker had no product-coverage
  analysis for tuple subjects, so it demanded a fall-through arm and blocked
  the build. A sound column-wise coverage check (`tuple_cases_cover`) now
  proves these exhaustive; it stays conservative (any arm shape it can't model
  bails to "not covered"), so non-exhaustive tuple matches — a variant left
  out, or a scalar column with no capture — still fire. `infer_expr_readonly`
  also learned to type a tuple subject so the exhaustiveness pass can see it.

### Fixed — `**kwargs` preserves call order (`tyc-vm`)

- The VM collected a `**kwargs` parameter into a `HashMap`, so the resulting
  dict's iteration / `repr` / serialisation order was nondeterministic.
  CPython preserves keyword-argument order; the VM now uses an insertion-
  ordered map (`IndexMap` + `shift_remove`) to match.

### Fixed — set operations on `dict` views (`tyc-types` + `tyc-vm`)

- **`d1.keys() - d2.keys()` no longer false-fires `tyc::operator_type_mismatch`
  (production blocker).** `dict.keys()` / `dict.items()` are set-like views
  that support `& | - ^`; the checker's set-difference carve-out only allowed
  `set`/`frozenset`, so `KeysView`/`ItemsView` operands were rejected and the
  build blocked. (`&`/`|`/`^` already type-checked.) The VM now also evaluates
  these (it previously raised `unsupported operand type(s)`), so `tyc run`
  matches the compiled path. `dict.values()` stays non-set-like, as in CPython.

### Added — VM `abc` module shim (`tyc-vm`)

- **`from abc import ABC, abstractmethod`** now works under `tyc run` (the
  default VM): `ABC` / `ABCMeta` are no-op bases and the `abstractmethod`
  family are identity decorators, so `class H(ABC): @abstractmethod def …`
  plus concrete subclasses run without falling back to `--compile`.

### Fixed — more VM parity (`tyc-vm`)

- **`del lst[i:j]` / `del lst[::k]`** (slice deletion) now works under
  `tyc run` instead of raising `slice expression outside subscript`.
- **User `__format__(self, spec)` is dispatched** by `f"{x:spec}"`,
  `"{:spec}".format(x)`, and `format(x, spec)` (previously the VM fell back to
  `__str__` and ignored the spec).
- **`str(KeyError("k"))` is `"'k'"`** (repr of the key) to match CPython's
  one builtin whose `str()` quotes its argument.

### Fixed — `bytes` operators in the VM (`tyc-vm`)

- `b"a" + b"b"` (concatenation) and `b"a" * 3` / `3 * b"a"` (repetition) now
  work under `tyc run` instead of raising `unsupported operand type(s)`.

The largest cluster was around the most common Python idiom the corpus
exercised that Typhon got wrong: **custom exception classes**.

### Fixed — `class FooError(Exception):` no longer breaks `raise FooError("msg")`

- **Exception subclasses are no longer auto-decorated with `@dataclass`
  (`tyc-desugar`).** A `class FooError(Exception): pass` emitted
  `@dataclasses.dataclass(slots=True)`, whose synthesised no-argument
  `__init__` shadows `BaseException.__init__`. The ubiquitous
  `raise FooError("message")` then died at runtime with
  `TypeError: FooError.__init__() takes 1 positional argument but 2 were
  given` — on both the compiled output *and* the VM. This affected every
  exception subclass: builtin bases (`Exception`, `ValueError`, `KeyError`,
  `Warning`, …) and user hierarchies (`AppError` → `NotFoundError`).

  Exception subclasses are now detected by base name (segment ending in
  `Error`/`Exception`/`Warning`, or an exact non-suffixed builtin like
  `BaseException`/`KeyboardInterrupt`) and lowered like `class!`: no
  `@dataclass`, and a `super().__init__(...)`-calling constructor synthesised
  **only** when the body declares fields. A field-less exception stays a bare
  `class FooError(Exception): pass` and inherits `BaseException.__init__`.
  Error-named classes with **no base** (Result error *variants*) are
  unaffected and keep their dataclass shape.

- **VM exception parity (`tyc-vm`).** The VM independently mis-modelled
  exception subclasses; `tyc run` now matches the compiled path:
  - field-less exception construction accepts positional args
    (`BaseException`-style, stashed as `.args`) instead of "takes 0 arguments";
  - `str(e)` / `repr(e)` render from `.args` (`str(FooError("x")) == "x"`,
    `repr == "FooError('x')"`) rather than the dataclass field form;
  - `except KeyError` catches a user `class MyKeyError(KeyError):` (builtin
    exception bases are recorded on the class, since the VM has no
    `Value::Class` for them);
  - a hand-written `super().__init__(msg)` in an exception `__init__` is
    captured so `str(e)` reflects the custom message;
  - missing builtin exceptions registered: `BaseException`, `Warning`
    (+ subclasses), `NameError`, `ImportError`, `UnicodeError`,
    `ConnectionError`, `IOError`, `KeyboardInterrupt`/`SystemExit`/
    `GeneratorExit`, and more.

  Known remaining VM-only limitations (compiled path is correct): `__cause__`
  is not tracked for `raise X from Y`, and CPython's `KeyError`-specific
  repr-quoting in `str()` is not reproduced.

Stress corpus and methodology: `stress/round-2026-06-16/`.

## 0.15.5 — 2026-06-15 — cross-module `extend BUILTIN:` propagation

A bugfix release that makes `extend BUILTIN:` work across module boundaries.
Previously, builtin extension methods (e.g. `extend str: def slug(...)`) only
rewrote call sites inside the declaring module — importing the module did not
carry the extension, so `title.slug()` in a consumer would fire
`tyc::attribute_not_found`. This release fixes the issue end-to-end across the
type checker, build/codegen, and VM runtime layers.

### Fixed — `extend BUILTIN:` now crosses module boundaries (#202)

- **A builtin extension declared in one module is now available to importers.**
  Previously `extend str: def slug(self) -> str: ...` in `textutil.ty` only
  rewrote `x.slug()` call sites *within* `textutil.ty`. Importing `textutil`
  from another module did nothing — `title.slug()` would fire
  `tyc::attribute_not_found` on `str`. The fix spans three layers:

  1. **Type checker (`tyc-db`):** `build_external_shapes` now seeds
     `__typhon_builtin_ext_*` sentinel class shapes from imported modules into
     the consumer's `ExternalShapes`, so `is_user_builtin_extension` recognises
     cross-module extension methods and suppresses `attribute_not_found`.

  2. **Build/codegen (`build.rs`):** After extracting local extensions, the
     build pass merges cross-module extension registries (keyed off
     `project_shapes`) before rewriting call sites. A new tracking variant
     (`rewrite_builtin_extension_calls_tracking`) reports which cross-module
     free functions were actually used, and explicit
     `from <module> import __typhon_ext_<TYPE>__<METHOD>` statements are
     injected so the emitted Python resolves at runtime.

  3. **VM runtime (`tyc-vm`):** The entry module is now pre-scanned for imports
     before builtin-extension rewriting. Sibling `.ty` files are parsed for
     their extension registries, which are merged into the local registry so
     `tyc run` rewrites cross-module call sites identically to `tyc build`.

- **New public API:** `rewrite_builtin_extension_calls_tracking` in
  `tyc-analyse` returns both the rewrite count and a `HashSet<String>` of used
  free-function names, enabling callers to inject only the required imports.

### Notes

- Full workspace suite green; new integration tests in `build_features`
  (`build_cross_module_extend_str_rewrites_consumer_call_site` and
  `check_cross_module_extend_str_no_attribute_not_found`). Clippy clean under
  `-D warnings`. No previously-accepted program changes behaviour — the fix is
  purely additive (call sites that previously errored now resolve).

## 0.15.4 — 2026-06-15 — cross-module interface conformance + `pub comptime let`

A bugfix release driven by a field report from building a layered FastAPI-style
app. It closes the cross-module structural-conformance gap the reviewer called
"the single biggest gap", fixes a `pub` modifier-stacking parse error, and
documents the module-local scope of `extend BUILTIN:`. Additive — every
previously-accepted program still type-checks identically, and `tyc build` /
`tyc run` output is byte-for-byte unchanged.

### Fixed — cross-module structural interface conformance

- **A concrete class that reaches a consumer module only *indirectly* now
  satisfies its interface.** When you "depend on abstractions across your
  package" you import the interface and a provider, never the concrete — so the
  concrete arrives as an imported provider's return type (`let r: Repo =
  get_repo()`) or behind a module-qualified annotation (`import m; r:
  m.Repo`), and was never seeded into the consumer's local `class_shapes`.
  Structural conformance then saw zero members and wrongly fired
  `tyc::interface_not_conforming` ("all members missing"), while the qualified
  form fell through to a nominal `tyc::type_mismatch`. The checker now resolves
  a class/interface shape through the project-wide module registry
  (`resolve_class_shape` / `interface_shape_for`) when it isn't locally seeded,
  so cross-module conformance matches same-module behaviour. Both the bare and
  the `mod.Iface`-qualified forms are recognised. (`tyc-types`.)
- **Soundness preserved:** the registry fallback genuinely checks members — a
  *non*-conforming concrete reached the same way still fires
  `tyc::interface_not_conforming`. Three regression tests cover the conforming
  bare path, the qualified path, and the non-conforming guard.

### Fixed — `pub comptime let` / `pub comptime def` parse error

- **`pub` now stacks with `comptime`.** The docs promise `pub` combines with
  every modifier, but `strip_pub_prefixes` didn't recognise the `comptime`
  forms, so the leading `pub ` survived, the comptime handler (which matches a
  line-start `comptime `) never fired, and the Python parser rejected
  `pub comptime let PORT: int = …`. `pub_decl_name` now recognises
  `comptime let` / `comptime mut` / `comptime def`, so the public name lands in
  `__all__` and the binding still inlines to a literal at build time.
  (`tyc-syntax`.) `pub lazy let` stays a clean parse error for now — its
  lowering runs in a separate text pass that would silently drop the laziness
  under a `pub ` prefix, so it is intentionally left unsupported rather than
  half-working.

### Documented — `extend BUILTIN:` is module-local

- **Clarified that a built-in extension only rewrites call sites inside its
  declaring module.** Unlike `extend ClassName:` (which merges into a user
  class and crosses module boundaries), `extend str:` is a purely static
  call-site rewrite keyed off the local block — importing the module does not
  carry the extension, so `title.slug()` in a consumer fires
  `tyc::attribute_not_found` on `str`. The guide, the bundled skill, and this
  changelog now spell this out and point at the free-function workaround
  (`pub def to_slug(s: str) -> str: …`, imported by name). No code change.

### Notes

- Full workspace suite green; new regression tests in `tyc-syntax`
  (`pub comptime let`) and `tyc-types` (cross-module conformance). No `.ty`
  program changes behaviour.

## 0.15.3 — 2026-06-15 — `tyc install skill` + bundled-skill refresh

A tooling release with no language, type-checker, VM, or emitted-runtime
change. It ships the `typhon` Claude skill *inside the compiler* and adds a
command to vendor it into any project, and brings the bundled skill current
with the v0.14.1 → v0.15.2 surface.

### Added — `tyc install skill`

- **A new `tyc install` subcommand materialises embedded tooling assets into
  a project; its first target is `skill`.** `tyc install skill` writes the
  whole `typhon` Claude skill tree — `SKILL.md`, the seven sibling reference
  docs (`REFERENCE.md`, `CLI.md`, `PITFALLS.md`, `DIAGNOSTICS.md`,
  `COOKBOOK.md`, `RUNTIME.md`, `PACKAGING.md`), and a new `references/` folder
  of 20 compile-clean example programs plus an index — into
  `.claude/skills/typhon/` of the current project.
- **The skill is embedded in the `tyc` binary at build time** via
  `include_str!` (manifest in `tyc/crates/tyc/src/commands/install.rs`), so the command
  works from any directory with no network access and no dependency on the
  Typhon source checkout. `tyc --version` identifies which snapshot you get.
- **Flags:** `--force` overwrites an existing copy (without it the command
  refuses when `.claude/skills/typhon/SKILL.md` already exists, so a
  customised copy is never clobbered); `--dir PATH` targets another project
  root; `--list` prints the files that would be written and exits without
  touching disk. Documented in `docs/cli.md` and the skill's own `CLI.md`.

### Added — `references/` examples in the bundled skill

- **20 curated, compile-clean single-file `.ty` programs** (one feature area
  each, hello-world through `as!`/`try_result` boundary casts), lifted
  verbatim from the `examples/` corpus, plus an index `README.md`. Each one
  type-checks under `tyc check` on this release.

### Changed — bundled skill brought current to v0.15.2

- **The bundled skill was stale at v0.14.0 and mislabelled later features.**
  The headline and additive-line range are updated to v0.15.2; phantom
  version tags (`v0.14.5` / `.6` / `.7` — releases that never existed) are
  corrected to **v0.15.0**, where `as!`-everywhere, `try_result`, and the
  compiler-bundled `.dty` stubs actually landed. Release-highlights
  subsections for v0.14.1 → v0.15.2 were added, and `tyc::gather_opportunity`,
  the `async_without_await` async-contract exemption, and `try_result` are now
  documented in the diagnostics and runtime references.

### Notes

- Full workspace suite green; `cargo fmt --check` and `cargo clippy` clean.
  No `.ty` program changes behaviour — `tyc build` / `tyc run` output is
  byte-for-byte unchanged from v0.15.2.

## 0.15.2 — 2026-06-14 — `as!` comment-awareness fix

A bugfix release with no language or API surface change.

### Fixed — quote in a comment no longer breaks a following `as!` / `?`

- **The `as!` checked-cast preprocessor mis-scanned a quote inside a `#`
  comment as a string opener.** `compute_code_skip_mask` (which marks the
  string/comment bytes the cast scanner skips) computed the string mask first —
  comment-blind — and only layered comment masking on afterwards. An apostrophe
  in a comment (`# assert each field's shape`) therefore opened a *phantom*
  string that ran to the next quote, swallowing an `as!` (or `?`) on a following
  line and surfacing as a spurious `tyc::parse` "Expected a statement" error.
  Multi-byte characters in such a comment (an em-dash `—`, a bullet `•`) shifted
  byte offsets the same way. The mask is now built in a **single unified pass**:
  a `#` outside a string starts a comment that runs to the newline, and quotes
  inside that comment are inert — so apostrophes, em-dashes, and `#` characters
  inside string literals all behave correctly. Three regression tests added in
  `tyc-syntax::preprocess`.

## 0.15.1 — 2026-06-14 — Compiler performance and docs-site accessibility

A performance and polish release with no language or API changes.

### Improved — Compiler performance

- **Source map generation now runs in O(N log N) instead of O(N²).** `build_source_map_v2`
  previously re-scanned the preprocessed source from the start for every token offset it
  needed to map to a line number. On a 10,000-line file this degraded to ~31 s; after
  precomputing newline positions in a single O(N) pass and resolving each offset with a
  binary search (`partition_point`), the same workload takes ~64 ms. The now-redundant
  `offset_to_line` helper has been removed.
- **Type-checker `cases_cover_type` no longer allocates on the heap for `Result` variant
  names.** The `["Ok".to_string(), "Err".to_string()]` array was replaced with `["Ok", "Err"]`
  (`&'static str`), eliminating two heap allocations per call in the exhaustiveness hot path.

### Improved — Docs-site accessibility

- **Keyboard focus is now visible on scrollable code blocks.** Starlight assigns
  `tabindex="0"` to horizontally-scrollable `<pre>` elements; those elements now receive the
  same 2 px accent-coloured focus ring as other interactive elements via
  `[tabindex="0"]:focus-visible` in `custom.css`.
- **Anchor-link navigation is now spatially oriented.** Clicking a Table of Contents entry
  briefly flashes the destination heading with a faded accent-colour halo
  (`@keyframes highlight-target`, 1.5 s ease-out). A `prefers-reduced-motion` fallback uses a
  static accent left-border instead of the animation.

## 0.15.0 — 2026-06-13 — `as!` everywhere, `try_result`, and compiler-bundled library stubs

A feature release sharpening Typhon at the library boundary, driven by a field
report from building a real async app. Four threads land together: the `as!`
checked cast now composes in any expression position, a `try_result` combinator
collapses the exception→`Result` boilerplate, the compiler ships curated `.dty`
stubs for the most-imported third-party libraries, and `async_without_await`
stops firing on methods that are async only to honour a contract. A cross-module
class-identity fix and a round of automated-review hardening round it out.

### Added — `try_result(thunk[, on_err])` exception→Result combinator

- **`try_result` bridges a library boundary into a `Result` in one expression**
  instead of a hand-written `try: return Ok(x) except E as e: return Err(...)`.
  It runs `thunk()` and returns `Ok(result)`; on any exception it returns
  `Err(on_err(exc))`, or `Err(exc)` (the raw exception) when no mapper is given:

  ```ty
  def load(path: str) -> Result[dict[str, str], str]:
      return try_result(lambda: read_json(path), lambda e: f"invalid JSON: {e}")
  ```

- A **prelude name** (no import in source, like `Ok`/`Err`/`Result`), typed as
  `Result[T, E]` — `T` from the thunk body, `E` from the mapper body
  (`Exception` when omitted). Special-cased in `infer_expr` like `as!`, so a
  wrong return annotation still fires `type_mismatch` (it's a real `Result`, not
  an `Any` escape hatch). `tyc build` auto-injects `from typhon_runtime import
  try_result`; the VM registers it as a prelude native that materialises the
  caught exception exactly as an `except E as e:` handler would (so
  `lambda e: str(e)` works under `tyc run`) and enforces its 1–2-arg arity.

### Added — compiler-bundled `.dty` stubs (httpx, requests)

- **`tyc` ships curated, embedded `.dty` stubs** for popular libraries whose
  packaging defeats venv introspection (httpx, requests to start), seeded into
  the project shape map by `tyc check` / `tyc build` / the LSP **before** venv
  enrichment — so an imported bundled library is shaped out of the box, its
  construction is type-checked, and its `unintrospectable-dependency` warning is
  suppressed, with no `.venv` or `tyc sync` required.
- **Gap-fill, not override**: an authored project `.dty`/`.ty` for the same
  module wins; the bundle in turn takes precedence over venv introspection.
  Class shapes are marked `partial` so members the stub omits stay lenient (no
  false `attribute_not_found`). Request methods take `**kwargs: object` and
  client constructors enumerate the common kwargs as optional fields. The long
  tail of dependencies stays best-effort; this is just the head. Lives in
  `tyc-db::seed_bundled_stubs` with the `.dty` text under `tyc-db/src/bundled/`.

### Changed — `as!` checked cast composes in any expression position

- **`EXPR as! TYPE` now lowers structurally** (a fixpoint rewrite with bracket-,
  string-, and comment-awareness) instead of line by line, so it composes
  wherever an expression can appear: nested in call arguments
  (`save(row[0] as! int, label)`), inside comprehensions / collection literals,
  across a multi-line value expression, and in statement conditions
  (`if raw as! bool:`). The left operand is the current syntactic slot (back to
  an enclosing bracket, a top-level `,`/`;`/`:`, an assignment, a
  `return`/`yield`/`if`/`while`/`assert` keyword, or the line start); the right
  operand is parsed as a type expression (dotted name, optional `[...]`
  subscript, `|`-union), so trailing code after the type stays outside the cast.
  A non-type right side is left for the parser to reject cleanly.
- The **VM intercepts `__typhon_checked_cast__` before argument evaluation** and
  evaluates only the value operand, so a cast to a union / parametric type
  (`x as! int | None`, `d as! dict[str, int]`) runs under `tyc run`.

### Changed — `async_without_await` understands async contracts

- An awaitless `async def` is **no longer warned when it is async only to honour
  a contract it can't opt out of** — implementing an async `interface` method
  (structural conformance) or overriding an async base-class method. A trivial
  `impl ConsoleSink: async def deliver(...)` satisfying `interface Sink: async
  def deliver(...)` now checks clean, removing the dead `await asyncio.sleep(0)`
  no-ops the diagnostic used to force. Gated on the *interface* method being
  async, so an `async` impl of a *sync* method — or any awaitless `async def`
  with no contract — still warns. (`MethodSig` gained an `is_async` flag;
  `method_satisfies_async_contract` does the check.)

### Fixed — qualified cross-module class references unify with their bare form

- **`import httpx; let r: httpx.Response = client.get(...)` no longer
  mismatches** the method's bare `Response` return (same for any project module:
  `import lib; let x: lib.Foo = lib.make()`). `Checker::is_assignable` unifies
  two class types whose final `.`-separated segments match **when at least one
  side is bare** — so a qualified reference unifies with the module's bare class,
  while two *different* qualified classes stay distinct (`httpx.Response` is not
  assignable to `requests.Response`; the bundled stubs additionally qualify
  their own return types so a cross-module mix-up is still caught). The
  relaxation only ever *adds* assignability between differently-spelled names —
  a genuine mismatch (`Response` vs `int`) is still caught.

### Notes

- The in-process VM already runs `async def` / `await` / `gather:` /
  `asyncio.run` via its cooperative-sequential scheduler (added earlier and
  documented in `docs/vm.md`); the skill reference's stale "synchronous-only"
  note was corrected.
- Hardening from the PR #195 automated review (Gemini / Codex / Copilot):
  statement-keyword handling for condition `as!` casts, lambda-default
  reference detection for runtime-import injection, `try_result` arity
  enforcement, the qualified↔bare bare-side guard, and cross-module stub
  identity — each with regression tests. Full workspace suite green; rustfmt +
  clippy clean.

## 0.14.3 — 2026-06-12 — LSP: live config refresh + committed end-to-end coverage

Follow-ups to the 0.14.2 editor work, both flagged in the 0.14.2 PR review.

### Changed — `typhon.toml` edits refresh editor diagnostics live

- **The language server now handles `workspace/didChangeWatchedFiles`.** The
  VS Code client already watches `**/typhon.toml` / `**/*.ty` / `**/*.dty`,
  but the server ignored the notification — so toggling `[strictness]
  suggest-gather` (or any knob) didn't take effect until you re-touched a
  source file. The server now invalidates its config cache on a
  `typhon.toml` change and re-checks every open document, so the editor
  reflects the new settings immediately. No VS Code extension change is
  needed — the existing file watcher already sends the notification.
- **The `[strictness]` lint knobs are now cached per project root**, keyed by
  the `typhon.toml` modification time, instead of re-parsed on every
  keystroke — closing the per-check disk-I/O concern from review. Because the
  cache is mtime-validated, the knobs stay fresh on an edit even for an editor
  that never sends `didChangeWatchedFiles`; the watcher just makes the refresh
  immediate for those that do.

### Added — committed end-to-end LSP tests

- Two `#[tokio::test]`s drive the real server over an in-memory pipe: one
  asserts `didOpen` of a file with an independent-await run publishes
  `tyc::gather_opportunity` at LSP severity `Hint`; the other asserts the
  watcher re-publishes the hint after a `typhon.toml` `suggest-gather`
  flip. The `didOpen → publishDiagnostics` path was previously only covered
  by unit tests plus a manual check.

## 0.14.2 — 2026-06-12 — Async concurrency: `gather_opportunity` advice + cross-module auto-gather

The single biggest "compile to the faster way" lever for async code is
concurrency: independent `await`s that run sequentially could run together.
Typhon could already *do* this (`gather:` / `auto-gather`), but `auto-gather`
is opt-in, requires a `@gatherable` decorator on every callee, and only
considered same-module `async def`s — so the most common real
missed-concurrency shape, two awaited method calls on an imported client,
was never surfaced. This release closes both gaps: the compiler now points
the opportunity out by default, and the `auto-gather` rewrite folds runs
that call `@gatherable` functions imported from another module.

### Added — `tyc::gather_opportunity` (advice, on by default)

- **`tyc check` and `tyc build` flag every run of 2+ adjacent independent
  awaited calls** inside an `async def` and suggest wrapping them in an
  explicit `gather:` block so they run concurrently in an `asyncio.TaskGroup`:

  ```ty
  async def load(client: Client, uid: int) -> tuple[User, list[Post]]:
      let user = await client.get_user(uid)        # advice: these 2 awaits
      let posts = await client.get_posts(uid)      # could run concurrently
      return (user, posts)
  ```

- **Callee-agnostic**, unlike `tyc::auto_gather_missed`: it fires for awaited
  **method calls on imported clients** (`await client.get_user(...)`), the
  shape `auto-gather` never touches, because the suggested fix — an explicit
  `gather:` block — works for any awaitable with no `@gatherable` decorator
  or `auto-gather` opt-in.
- **Sound and conservative.** Independence is decided by static data flow:
  a run breaks the moment a later await references a name bound earlier —
  including through the callee's *receiver* (`b = await a.next()`), keyword
  args, comprehensions, walrus, slices, and f-strings (all reused from the
  `auto-gather` independence analysis). A single non-matching statement
  between two awaits ends the run. It **never rewrites** — concurrency is a
  behaviour change the author opts into, since data-flow independence does
  not rule out ordering side effects — and it is **advice-level**, so it
  never blocks a build. Verified to fire zero times across the 15-app
  example corpus (which already gathers where appropriate), so default-on
  adds no noise.
- **Knob:** `[strictness] suggest-gather` (default `true`). Set `false` to
  silence the nudge project-wide. When `auto-gather` is also on, runs it
  folds into a `TaskGroup` are gone before this pass runs, so they aren't
  double-reported. `tyc explain gather_opportunity` documents it offline.

### Changed — `auto-gather` now folds imported `@gatherable` callees

- **`[strictness] auto-gather` extends across module boundaries.** A run of
  independent awaits whose callees are `@gatherable` async functions
  **imported from another project module** now folds into an
  `asyncio.TaskGroup`, exactly like a same-module run:

  ```ty
  # services.ty
  @gatherable
  pub async def fetch_user(uid: int) -> User: ...
  @gatherable
  pub async def fetch_posts(uid: int) -> list[Post]: ...

  # main.ty
  from services import fetch_user, fetch_posts
  async def load(uid: int) -> Dashboard:
      let u = await fetch_user(uid)     # folded into one TaskGroup …
      let p = await fetch_posts(uid)    # … because both are @gatherable in `services`
      return Dashboard(u, p)
  ```

- Each module's set of `@gatherable` async functions is published on
  `ModuleShapes` (the same cross-module registry that carries class shapes,
  newtypes, enums, …); the build pass resolves every `from M import name`
  through the resolver's `import_info` (correct relative / absolute / alias
  handling, shared with the type checker) and seeds the imported local name
  into the auto-gather eligibility set.
- **The safety boundary holds across modules:** the `@gatherable` decorator
  is still required — an imported async callee *without* it is never folded,
  so the author's concurrency attestation is respected regardless of which
  module the function lives in. Mis-resolution can only ever *fail* to fold
  (a missed optimisation), never fold something un-attested.

### Added — advisory lints surface live in the editor (LSP)

- **The advisory lints now appear as you type, not just in CI.** The pure-AST
  advisories — `gather_opportunity`, `mutable_default_param`,
  `empty_collection_no_annotation`, `typing_alias_in_annotation`,
  `is`-literal, loop-closure-capture, and the inline secret-literal lint —
  previously only ran inside the `tyc check` / `tyc build` commands, so they
  were invisible in the editor. They now flow through the language server too.
- A single shared `editor_lint_diagnostics` is the source of truth for the
  advisory set, called by both `tyc check` and the LSP, so the editor and CI
  can never drift. The LSP reads `[strictness] suggest-gather` /
  `allow-secret-comptime` from the project's `typhon.toml`, so silencing a
  lint silences it in the editor as well.
- Advice-severity diagnostics render as unobtrusive **hints** (the editor's
  faint underline) rather than warnings, matching the terminal's `☞` badge.
- No editor-side change is required: the VS Code extension is a thin
  pass-through language client, so updating the `tyc` binary is enough to
  light the hints up.

## 0.14.1 — 2026-06-12 — Cross-module shape propagation completeness

A dogfooding round — a self-hosted API budget tracker built end-to-end on
v0.14.0 — surfaced a class of defect: several per-module checker tables were
never threaded through the cross-module shape registry, so type information
the in-module checker relies on silently went missing once a declaration was
consumed from another module. Four maps had the gap — `newtypes`,
transparent `type_aliases`, `enums`, and `frozen_classes` — all now carried
through the same path that already propagated `sealed_unions`, `interfaces`,
and `class_shapes`. Additive — every previously-accepted program type-checks
identically, the full workspace suite stays green, and all 15 example apps
(which declare newtypes/enums/frozen classes in a `domain/` module and import
them widely) check clean.

The shared fix threads each map through the four-stage cross-module path
(`extract_module_shapes` → `ModuleShapes` → `build_external_shapes` →
`check_module_with_imports`), seeding the consumer both when the name is
imported directly (alias-aware) and when it's reached *only* through an
imported class field / function parameter / return type (the case that
actually bit in each instance). A local declaration always wins on a name
collision with an imported one.

### Fixed — newtype escape-upward across module boundaries

- **An imported `newtype` now widens to its base type.** `newtype
  ProjectTag = str` in `models.ty` plus `let key: str = spend.project`
  (where `spend.project: ProjectTag`) in a consumer wrongly fired
  `tyc::type_mismatch` — the consumer's `newtypes` table had no imported
  base to unwrap to. The reverse direction stays sound and is now *more*
  precise: a bare base into an imported-newtype slot surfaces the dedicated
  `tyc::newtype_violation` ("wrap with `ProjectTag(...)`") instead of a
  generic mismatch, matching the in-module diagnostic.

### Fixed — transparent type aliases across module boundaries

- **An imported `type Report = ReportData` now unwraps in the consumer.**
  A `ReportData` value flowing into a `Report`-typed slot (or vice-versa)
  wrongly fired `tyc::type_mismatch` because the consumer's `type_aliases`
  table never received the imported alias, so `unwrap_alias` had nothing to
  resolve `Report` through. Sealed-union aliases are excluded (they already
  ride `sealed_unions`); only transparent aliases are published.

### Fixed — enum exhaustiveness across module boundaries

- **An exhaustive `match` over an imported `enum` no longer fires a false
  `tyc::missing_return`.** The consumer never saw the enum's closed member
  set, so it couldn't prove a complete `case Color.RED / GREEN / BLUE:`
  match exhaustive and assumed a fall-through. The enum's ordered member
  list is now published and seeded.

### Fixed — frozen-class writes across module boundaries (soundness)

- **A field write on an *imported* `frozen` class now trips
  `tyc::frozen_assign`.** Previously the consumer silently accepted
  `c.port = 9999` on an imported `class Config frozen:`, which raises
  `FrozenInstanceError` at runtime — a cross-module soundness gap.
  Frozen-ness is preprocessor line-based (the `frozen` modifier is stripped
  before parsing), so it's filled into `ModuleShapes` by the callers that
  hold the preprocess metadata via a new `tyc_types::frozen_class_names`
  helper, rather than from the AST-only `extract_module_shapes`.

### Tests

- `tyc-types`: `cross_module_newtype_widens_to_base`,
  `cross_module_bare_base_into_newtype_still_rejected`.
- `tyc-db` (full-pipeline, via the project shape registry):
  `cross_module_newtype_field_widens_without_importing_newtype`,
  `cross_module_imported_newtype_name_widens_to_base`,
  `cross_module_transparent_type_alias_unwraps`,
  `cross_module_enum_exhaustive_match_no_false_missing_return`,
  `cross_module_frozen_class_field_write_errors`.

## 0.14.0 — 2026-06-12 — `as!` checked boundary cast + auto traceback remap

Two ergonomics features motivated by a playground retrospective: the
`unsafe:`-block-plus-re-assertion dance at every untyped boundary was the
sharpest remaining ceremony, and runtime tracebacks pointing at emitted
`.py` (rather than `.ty`) source was the sharpest debugging tax. Both are
additive — every previously-accepted program type-checks identically — and
the full workspace suite plus the example corpus stay green.

### Added — `as!` checked boundary cast

- **`EXPR as! TYPE`** is the one-line, *sound* replacement for the
  `unsafe:` + re-assert idiom at an untyped boundary:

  ```python
  let data = resp.json() as! dict[str, int]   # was: unsafe: … then re-assert
  let uid  = row[0] as! int
  ```

  The checker types the expression as `TYPE` (so the boundary value — which
  may be `Any` — is accepted *without* an `unsafe:` block), and the cast
  lowers to a runtime guard `checked_cast(EXPR, TYPE)` in the generated
  `typhon_runtime/cast.py` that verifies the value's shape against `TYPE`
  and raises `TypeError` on a mismatch. Unlike a static-only re-assertion
  (which trusts the boundary blindly) or TypeScript's unchecked `as`, this
  is genuinely *checked* — the structural check recurses through
  parameterised containers (`list[int]`, `dict[str, int]`, `tuple[int, ...]`,
  unions, `Optional`), so a JSON payload that is a `list` where you claimed
  `dict[str, int]`, or a `dict[str, str]` where you claimed `dict[str, int]`,
  is rejected at the boundary. `Any` / `object` targets and shapes the guard
  can't model fall back to acceptance, so an `as!` can only reject values it
  can prove wrong.
- v1 scope: `as!` casts the entire preceding value expression on a single
  physical line, in a value position (after `=` / `return` / `yield`, or as
  a bare expression). It rides the existing `tyc-syntax` lowering pipeline
  (`tyc-types` reads the target via the same path as `type[T]` inference, so
  no special generic machinery was needed), the in-process VM treats it as
  an identity passthrough (the authoritative structural check runs on the
  `tyc build && python` path), and `tyc fmt` preserves the surface syntax.
  A nested-in-call-arguments or multi-line `as!` is left for the AST-based
  lowering migration; until then the parser surfaces a clean error.

### Added — `[emit] traceback-remap`

- **`[emit] traceback-remap = true`** (default `false`) injects a
  `typhon_runtime.traceback.install()` call into the entry module's
  `if __name__ == "__main__":` block. The installed `sys.excepthook` reads
  the emitted `.py.map` sidecars and rewrites an uncaught exception's
  traceback to point at `.ty` source — the same mapping `tyc trace` applies,
  but automatically and with no manual step. It only touches the entry
  script (library imports never trip the `__main__` guard) and falls back to
  the previous hook on any failure, so it can only improve a traceback,
  never suppress one. Default-off keeps existing projects and runtime-free
  entry points byte-for-byte unchanged.

## 0.13.2 — 2026-06-12 — playground stress round (method-call `match`, multi-line `?`, `gather:`-in-`match` import, `fmt` round-trips)

### Fixed — playground stress round (method-call `match`, multi-line `?`, `gather:` import injection, `fmt` round-trips)

A round of app-building against v0.13.1 surfaced four defects, all fixed
here. The full workspace suite and the example corpus stay green.

- **`tyc::missing_return` false-fired on a `match` over a direct
  impl-method call returning `Result[T, E]`.** `match b.fetch(): case
  Ok(v): … case Err(e): …` (with every arm returning or raising) tripped
  `missing_return`, while the same match over a free-function call or a
  bound `let` name passed. The read-only inference that feeds the
  exhaustiveness pass typed a non-`@property` bound-method *value*
  (`b.fetch`) as `Unknown`, so the wrapping call's `Result` return type
  never reached the coverage check. A bound method accessed as a value is
  now modelled as a `Function` carrying the method's return type, so a
  call expression (`b.fetch()`) recovers it — bringing method-call match
  subjects to parity with free-function-call subjects.
- **`?` after a multi-line call expression failed to parse.** A
  propagation `?` on the physical line that closes a multi-line call —
  `let total: int = add(` / `    1, 2` / `)?` — lowered the lone `)?` line
  to `__typhon_q_0__ = )` and surfaced a spurious `tyc::parse`
  ("Expected `,`, found name"). The `?` lowering now collapses a statement
  whose trailing `?` sits on a later physical line than the start of its
  expression onto one logical line first (joining with single spaces so
  adjacent tokens never fuse, and blanking the consumed continuation lines
  to keep line indices stable), so the existing single-line logic handles
  it. Single-line `?`, plain multi-line calls, and `T?` annotations
  (including multi-line ones) are untouched.
- **`gather:` inside a `case` arm emitted `asyncio.TaskGroup()` without
  injecting `import asyncio`.** The desugar pass that decides whether to
  inject `import asyncio` walked `if` / `for` / `while` / `with` / `try`
  bodies but not `match` arms, so a `gather:` nested in a `case` produced a
  module that `NameError`ed at runtime. The asyncio-usage walker now
  descends into `match` subjects, guards, and arm bodies (and, while there,
  into the `except` handlers of a `try`, which it had also skipped).
  Top-level and `if`-nested `gather:` were already correct, and the
  de-dupe against an existing `import asyncio` is unchanged (no
  double-injection).
- **`tyc fmt` diverged from `tyc check`'s grammar and corrupted multi-line
  `freeze let`.** Two formatter defects: (1) the validation parse rejected
  the typed tuple-unpack form `let (a: float, b: float) = pair()` that
  `tyc check` / `tyc build` accept, because the formatter's validation
  pipeline omitted `expand_typed_let_unpack`; (2) reformatting a
  **multi-line** `freeze let X = {` … `}` baked the internal
  `__typhon_freeze__(` lowering wrapper into the `.ty` source — the
  single-line restorer couldn't reverse the unclosed opener, and the `)`
  appended to the closing line was never removed. The validation pipeline
  now expands typed tuple-unpack, and the keyword-restoration pass reverses
  the multi-line wrapper (stripping the `__typhon_freeze__(` opener and the
  matching appended `)`, located by a bracket-depth walk). Single-line
  `freeze let` round-trips and the build/freeze lowering are unchanged.

## 0.13.1 — 2026-06-11 — playground stress round (async await-propagation, Task await-unwrap, VM project run, fmt, pub enum)

### Fixed — playground stress round (async `?`, await-unwrap, VM project run, `fmt`, `pub enum`)

A round of app-building against v0.13.0 surfaced six defects, all fixed
here; a follow-up PR review hardened two of them (see the `await`-unwrap
and `type`-alias bullets). The full workspace suite and the example corpus
stay green.

- **`?` on a bare `async` call silently miscompiled instead of erroring.**
  Applying `?` directly to an un-awaited `async def` call
  (`let v: int = inner(n)?`) desugared to a `.value` read off the
  *coroutine* — `tyc check` passed, then the program crashed at runtime
  with `AttributeError: 'coroutine' object has no attribute 'value'`. The
  checker now fires `tyc::missing_await` on a `?`-propagation temporary
  whose operand is an un-awaited async call, even inside an `async def`
  body, steering the user to the documented `await inner(n)?` idiom (which
  was, and stays, correct). The check is scoped to `?` / with-chain
  propagation temporaries and exempts anything already inside `await`, so
  `gather:` / `go` / `asyncio.create_task` patterns are unaffected.
- **`await` didn't unwrap a stored `asyncio.Task[T]` / `Future[T]`.** A
  `go work() -> t` handle parked in an explicitly-annotated
  `list[asyncio.Task[int]]` and awaited in a loop typed `await t` as
  `Task[int]`, firing a false `tyc::type_mismatch`. The await-unwrap table
  now covers `Task[T]` and `Future[T]` alongside `Awaitable` / `Coroutine`.
  The unwrap is keyed on the bare generic name, so it's **suppressed when
  the project defines its own class of that name** — a non-awaitable user
  `Task` / `Future` is left untouched (PR #187 review).
- **`tyc run` (VM) gating check treated the entry file as standalone.**
  The pre-run `tyc check` ran on `main.ty` alone, so every
  `from sibling import …` fired `tyc::unknown_module` plus knock-on false
  errors (e.g. an exhaustive `match` over an imported sealed union
  degrading to `tyc::missing_return`) that blocked execution even when
  `tyc check src/` was green. The pre-run check now resolves the whole
  project `src` tree when the entry lives inside it.
- **VM didn't bind a `type` sealed-union alias as a module attribute.**
  `from mod import Event` for `pub type Event = A | B | C` raised
  `AttributeError: module '…' has no attribute 'Event'` under `tyc run`
  (the build path emits the alias and CPython binds it lazily, so
  `tyc run --compile` already worked). The VM now binds the alias name: a
  union RHS lowers to a tuple of its member types (a valid `isinstance`
  argument); other shapes evaluate directly; an unevaluable RHS falls
  back to a name placeholder, mirroring CPython's deferred evaluation.
  **Forward-declared aliases** (written above their variant classes) are
  re-resolved after the module body runs — so a cross-module
  `from mod import AB` reads the real value — and forced on demand in
  `isinstance`, so same-module runtime use during body execution resolves
  correctly too, matching CPython's lazy `TypeAliasType` (PR #187 review).
- **`tyc fmt` left desugar syntax in a `freeze let` whose value held a
  `#`.** Restoring `freeze let X = […]` split the line on the first `#`
  without string-awareness, so a `#` *inside a string literal* in the
  value was mistaken for a comment and the `__typhon_freeze__(...)`
  wrapper failed to strip — leaving non-source syntax in the formatted
  file (a regression of the 0.9.x `fmt` corruption class). The
  originally-suspected non-ASCII trigger was a mis-attribution; the
  operative trigger is the in-string `#`. The reverse comment scan is now
  string-aware, matching the forward wrap path.
- **`pub enum` did not parse.** `pub` stacked with every declaration form
  except `enum` (`pub enum Species:` errored with "Simple statements must
  be separated by newlines or semicolons"). `enum` is now recognised by
  the `pub` pre-pass, so `pub enum` parses, contributes to `__all__`, and
  round-trips through `tyc fmt` like every other `pub`-prefixed form.

## 0.13.0 — 2026-06-11 — stress-round fixes + post-release code review: cross-module extend, dict-literal lowering, enum exhaustiveness, Result API

A fresh adversarial sweep (~35 programs across scripts, IO, data
structures, async, ML, AI/agents, APIs, and SDKs, each run through both
`tyc run` and `tyc build` + CPython with output diffing) surfaced two
silent-wrong-output defects on documented features, several type-system
blind spots, and a batch of VM coverage gaps. All fixed below; the full
workspace suite and the 254-file example corpus stay green.

### Fixed — post-release code review

A line-by-line review of everything that landed since v0.12.0 turned up
ten issues, all fixed here:

- **`random.sample` diverged from CPython for seeded runs.** The
  selection-set-vs-pool threshold computed `4 ** ceil(log3(k))` instead
  of CPython's `4 ** ceil(log(k*3, 4))` (log base 4 of `3·k`), so for a
  broad range of `(n, k)` the two implementations took different
  branches and consumed the MT19937 stream differently — breaking the
  seeded-reproducibility guarantee against `tyc build` + CPython. Now
  exact.
- **`incompatible_override` false-positived on valid LSP widening.** The
  check compared total parameter counts, so an override that added an
  *optional* parameter (`def handle(self, event)` →
  `def handle(self, event, retries=3)`) was wrongly flagged. It now
  compares required-vs-accepted argument ranges: an override is flagged
  only when it requires more arguments than the base, accepts fewer, or
  adds a required keyword-only parameter.
- **VM injected `self` into `@staticmethod` / `@classmethod` read
  through an instance.** Static/class methods reaching a class via a
  cross-module `extend` block (lowered to `Cls.m = fn`) were bound as
  ordinary instance methods, prepending the instance as a spurious first
  argument. The `@staticmethod` / `@classmethod` markers are now carried
  on the function value itself and honoured wherever the function is
  read, which also fixes the pre-existing case of a static method called
  through an instance.
- **`incompatible_override` warnings on nested classes anchored at the
  start of the file.** The class-name span map only scanned top-level
  statements; a subclass defined inside a function or another class fell
  back to byte offset `(0, 1)`. The span scan now recurses through
  nested bodies.
- **Quoted literal-singleton union members that collide with a builtin
  type name lost their meaning.** With quoted annotations now resolved
  as forward references, `type Mode = "int" | "str"` resolved its
  members to the structural types `int` / `str`. Bare quoted scalar
  builtin names (`"int"`, `"str"`, `"None"`, …) now stay literal
  singletons; class-name and subscripted forms still resolve as forward
  references, matching standard Python.
- **`sys.stdout.write()` / `Path.write_text()` returned the UTF-8 byte
  length** instead of CPython's character count, so the return value
  diverged for non-ASCII text. Both now return `chars().count()`.
- **`random.randrange()` rejected descending ranges.** `randrange(10, 0,
  -1)` raised "empty range" instead of selecting from `10, 9, …, 1`.
  Negative steps now follow CPython's width computation; a zero step
  raises "zero step".
- **`loop_closure_capture` missed late-binding captures in `try`/`else`/
  `finally` and `match` blocks** nested in a loop. The lint's statement
  walkers now recurse through those bodies (and loop `else` clauses) too.
- **The sequential-loop `let` carve-out suppressed legitimate post-loop
  shadows.** A `let` declared in a loop body was marked loop-origin
  permanently, so re-declaring the name *after* the loop slipped past
  `no_block_shadow`. The carve-out now only applies while resolving
  inside a loop body; the "next loop reuses the scratch name" pattern
  stays silent as before.
- **`OrderedDict.move_to_end(key, last=False)` rebuilt the whole map.**
  Each front-move reallocated and re-inserted every entry (O(n) per
  call, O(n²) over an LRU loop). It now uses an in-place
  `shift_insert(0, …)`.

### Fixed — silent wrong output

- **Cross-module `extend ClassName:` silently dropped its methods.** The
  desugar merge only handled same-file targets; an imported target's
  pseudo-class was removed and the methods discarded, so a clean build
  crashed with `AttributeError` at first call. Foreign-target blocks now
  lower in place to module-level functions plus class-attribute patches
  (`Record.label = __typhon_extend_Record__label`, decorators
  preserved), which work on `slots=True` / frozen dataclasses and
  third-party classes alike. The three front-ends also agree again:
  `check` no longer mislabels the import unused, `run` no longer fires
  `impl_unknown_class` on imported targets, and the VM binds
  function-valued class attributes as methods (descriptor semantics).
- **TypedDict-style dict literals against `class` / `model` annotations
  never lowered.** `let u: User = {"id": 1, "name": "ada"}` has
  type-checked since v0.3.0 but emitted the raw dict — `u.name` then
  crashed at runtime in both modes. A new early desugar pass rewrites
  the literal to `User(id=1, name="ada")` for local classes/models,
  recursing into class-typed fields for nested initialisation.
- **`class! X(enum.StrEnum):` emitted a corrupted enum** (synthesised
  `__init__` + member definitions stripped as field defaults →
  import-time `TypeError`). `class!` now skips constructor synthesis
  for enum-family bases.
- **VM: `StrEnum` / `IntEnum` members now behave as their value**
  (CPython 3.11+ semantics): `Status.ACTIVE == "active"` is `True`,
  ordering/arithmetic unwrap, `str()` renders the value, and dict/set
  hashing flows through the value. Previously equality was silently
  `False`.
- **VM: `@memo` was a no-op** — the desugar pass that lowers it to
  `@functools.cache` only ran on the build path, so memoised recursion
  ran exponentially under `tyc run` (`fib(60)` effectively hung; it now
  returns instantly). Sibling-module loading also now threads the
  class-kind markers (`frozen` / `plain` / `class!`) and `pub` names.

### Added — language / checker

- **Enum match exhaustiveness.** An enum's member set is a closed set:
  covering every member with `case Enum.MEMBER:` arms (or `|`
  or-patterns) satisfies the return-path analysis, and a missing member
  fires `tyc::non_exhaustive_match` naming it — previously both shapes
  collapsed into a generic `missing_return`.
- **Quoted annotations resolve as forward references.** `next: "Node"`,
  `-> "Tree[T]"`, `"list[Node]"` — the reflexive Python idiom — now
  parse and resolve (including a trailing `?`); previously they became
  string-literal singleton types with baffling mismatches. The
  literal-union form `type Color = "red" | "green"` is unchanged.
- **Generic interface conformance.** `MemRepo[int]` now structurally
  satisfies `interface Repo[T]` as `Repo[int]` (pairwise-assignable
  type arguments; mixed bare/generic arities conform at head level).
- **Match-arm scrutinee narrowing.** `case Action(_, _):` narrows the
  subject variable to `Action` inside the arm (or-patterns narrow to
  the union; builtin patterns to the builtin type), gated on the
  narrowed type provably flowing back into the declared type.
- **Exhaustiveness for expression scrutinees.** `match items[-1]:` over
  a sealed union now runs the same analysis as a plain-name subject
  (subscript element types resolve through `infer_expr_readonly`).
- **`Result` unwrap family.** `unwrap()`, `expect(msg)`,
  `unwrap_or(default)`, `unwrap_or_else(f)`, `ok()` (→ `T?`), `err()`
  (→ `E?`), `is_ok()`, `is_err()` on `Ok` / `Err` / `Result` — runtime
  template, VM natives, and receiver-narrowed checker signatures. The
  Result method surface is now *closed*: an unknown method fires
  `attribute_not_found` at check time (previously `.unwrap()` passed
  the checker and crashed at runtime).
- **Lambda arity checking.** Lambdas infer as `Type::Function` carrying
  their arity, so `apply(lambda x: x)` against `Callable[[int, int],
  int]` is a check-time mismatch; defaults / `*args` mark the lambda
  variadic.
- **Recursive-type groundwork: sequential-loop `let` carve-out.** Two
  sibling `for` / `while` loops can re-use the same body-scoped `let`
  scratch name (mirrors the sibling `if`-arm / `case`-arm carve-outs);
  shadowing a live outer `let` still errors.
- **New lint `tyc::mutable_default_param`** (warn): a mutable literal /
  constructor as a function parameter default is created once and
  shared across calls — class fields already got the `default_factory`
  rewrite, parameters now get the warning.
- **New lint `tyc::is_literal_comparison`** (warn): `s is "x"` compares
  identity against a literal (CPython itself SyntaxWarns).
- **Builtin exception table completed.** `TimeoutError`, `EOFError`,
  `ConnectionResetError`, `BrokenPipeError`, `BlockingIOError`,
  `IsADirectoryError`, `BufferError`, `ReferenceError`, the warning
  categories, and friends no longer fire `unknown_name` in `except`
  clauses.

### Added — second sweep (closing the remaining items)

- **VM cooperative asyncio.** `async def` calls produce coroutine thunks
  (the body runs when driven, matching CPython's contract); `await` /
  `asyncio.run` / `gather` (incl. `return_exceptions=True`) /
  `TaskGroup.create_task` / `wait_for` / `spawn` (the `go` lowering)
  force them in program order. `gather:` fan-out, `go ... -> task` +
  `await task`, `async for` / async comprehensions, `async with`,
  `asyncio.sleep`, `asyncio.timeout` (wall-clock-checked at scope exit),
  and `asyncio.Queue` all run — with output identical to CPython unless
  correctness depends on task *interleaving*, in which case `Queue`
  operations raise a `RuntimeError` naming `tyc run --compile` instead
  of deadlocking.
- **VM traceback frames.** Runtime errors under `tyc run` now render a
  CPython-style frame chain (`File "x.ty", line N, in fn` + source
  line) pointing directly at the `.ty` file — previously the traceback
  carried no frames at all. `//` / `%` by zero match CPython's message.
- **CPython-exact `random`.** The VM's `random` module is now a faithful
  MT19937 (CPython seeding, `random()`, `getrandbits` / `_randbelow`,
  and the exact `gauss` / `shuffle` / `sample` / `choice` algorithms) —
  seeded programs produce byte-identical sequences across `tyc run` and
  `tyc build` + CPython.
- **Recursive type aliases.** `type Json = None | bool | int | float |
  str | list[Json] | dict[str, Json]` is now legal — `cyclic_type_alias`
  only fires for cycles with no type constructor anywhere (no base
  case). Container literals resolve their element expectations through
  aliases and unions, so nested `Json` literals check.
- **`tyc::incompatible_override`** (warn): a subclass method overriding
  a base method with different arity, a narrower parameter, or an
  unassignable return — the LSP violation mypy/pyright flag.
- **`tyc::loop_closure_capture`** (warn): a closure created in a loop
  referencing the loop variable observes the *final* value at call time
  (`[lambda: i ...]` → `[2, 2, 2]`); the `lambda i=i:` idiom and
  immediately-invoked closures are exempt.
- **`tyc migrate`** now relocates class-body methods into `impl` blocks,
  rewrites `class X(Enum):` to the `enum` keyword, simplifies
  `field(default_factory=...)` to bare-literal sugar, and prunes the
  imports those rewrites orphan — migrated output checks with zero
  errors *and zero warnings*.
- **`?`-in-f-string** parse failures now carry a targeted hint (bind
  with `let v = fallible()?` first); `.dty` stubs count as project
  modules for import vetting; the no-arg `tyc trace` test can no longer
  hang on an inherited stdin pipe.

### Added — CLI / emit / VM

- **`tyc run --compile script.ty`** synthesises a throwaway scaffold
  (src/main.ty + minimal typhon.toml, `--temp` semantics, `uv sync`
  skipped) so single-file scripts build-and-execute without `tyc init`;
  `tyc build script.ty` now errors actionably instead of reporting
  "'script.ty/src' does not exist".
- **Emitted Python now imports `collections.abc` names it uses.**
  Annotations using checker-prelude names (`Iterator`, `Sequence`,
  `Callable`, …) previously only survived via `from __future__ import
  annotations`; runtime annotation resolution (`typing.get_type_hints`,
  FastAPI DI, pydantic) raised `NameError`. The VM also gained a
  `collections.abc` shim, so the idiomatic import no longer breaks
  `tyc run`.
- **VM stdlib fills:** `print(sep= / end= / file=sys.stderr / flush=)`
  with real `sys.stdout` / `sys.stderr` stream objects; `sum(start=)`;
  `random.uniform / gauss / randrange / choice / shuffle / sample`;
  `os.getcwd / remove / unlink / rmdir / mkdir / makedirs / rename /
  listdir` and `os.path.join / basename / dirname` (IO errors map to
  the matching Python exception types); `pathlib.Path.iterdir / glob /
  mkdir(parents=, exist_ok=) / unlink / read_bytes / write_bytes`;
  `dict.popitem(last=)` and `OrderedDict.move_to_end`; unary dunders
  (`__neg__` / `__pos__` / `__invert__`, `abs()` → `__abs__`).

## 0.12.0 — 2026-06-07 — VM `__lt__` parity, dict/str builtins, and deep library introspection

Adversarial stress-round follow-up against v0.11.0. A fresh sweep across
simple scripts, file/JSON I/O, ML-shaped numerics, agent/tool-dispatch
loops, SDK patterns, and multi-file projects surfaced six defects where
`tyc run` (the VM) silently diverged from `tyc build` + CPython, plus one
type-checker resolution gap. The build path and the type checker were
otherwise solid across the sweep (soundness probes for nullable misuse,
list invariance, newtype bypass, frozen reassignment, Result error-type
mismatch, and interface conformance were all correctly rejected).

A follow-up VM-vs-CPython differential review found four more parity
defects — `sorted` / `min` / `max` ignoring a user `__lt__` (silent wrong
output), and missing `dict.popitem` / `dict.fromkeys` / `str.translate` /
`str.maketrans` — all fixed below. The same pass widened the third-party
introspection used by `tyc check` / `tyc build` to capture parameter and
return *annotations*, so fully-typed pure-Python dependencies now get
argument-*type* checking (not just arity) at the call site. (The
remaining VM coroutine-generator limit and the introspection scope are
now documented rather than fixed — see `docs/vm.md` and
`docs/diagnostics/missing_argument.md`.)

### Fixed — VM / CPython parity

- **`str.replace(old, new, count)` honours the third `count` argument.**
  The VM ignored it entirely, so `"aaaa".replace("a", "b", 2)` returned
  `"bbbb"` instead of `"bbaa"` (and `count=0` was not the documented
  no-op). A negative count means "replace all", matching CPython.
- **`sorted(..., reverse=True)` and `list.sort(reverse=True)` are stable.**
  Both lowered to a stable ascending sort followed by an unconditional
  `.reverse()`, which flips the order of equal-key elements. They now
  reverse the *comparator* instead of the list, so equal keys keep their
  original relative order as CPython guarantees.
- **`json.dumps(obj, sort_keys=True)` sorts keys.** The kwarg was parsed
  and discarded; output stayed in insertion order. `sort_keys` now
  threads through both the compact and `indent=` serialisers recursively.
- **`math.isnan` / `math.isinf` / `math.isfinite` exist.** They were
  absent from the VM's `math` shim and raised `AttributeError`.
- **Float-presentation format types coerce int/bool operands.** `f"{42:.2f}"`
  printed `42` (the spec was dropped); `f"{n:e}"` on an int printed the
  bare integer. `e`/`E`/`f`/`F`/`g`/`G` now promote an int/bool operand to
  float before formatting, matching CPython.

### Fixed — VM comparison protocol (`sorted` / `min` / `max`)

- **`sorted()` / `min()` / `max()` honour a user `__lt__`.** They compared
  via the dunder-blind `Value::py_cmp`, which returns "equal" for class
  instances — so `sorted([R(1), R(3), R(2)])` with a custom `__lt__`
  returned the list *unsorted* and `min` returned the first element rather
  than the smallest. They now route through `Interpreter::value_cmp` (the
  same path `list.sort()` already used), so a user comparison dunder takes
  effect and the VM matches CPython. This was a silent-wrong-output defect
  in the same class as the `sorted(reverse=True)` stability bug fixed
  earlier this cycle. Covers both the no-kwarg native path and the
  `key=` / `reverse=` / `default=` keyword path.

### Added — VM builtin methods (`dict` / `str`)

- **`dict.popitem()`** removes and returns the last inserted `(key, value)`
  pair (LIFO, matching CPython 3.7+); raises `KeyError` on an empty dict.
  Previously `AttributeError: dict has no method 'popitem'`.
- **`dict.fromkeys(iterable[, value])`** builds a new dict with each key
  from `iterable` mapped to `value` (default `None`). Previously crashed.
- **`str.maketrans(x[, y[, z]])`** and **`str.translate(table)`** build and
  apply a translation table (dict / two-string / delete-string forms).
  Previously `AttributeError: 'str' object has no attribute 'translate'`.

### Added — third-party argument-*type* checking (annotation capture)

- **Venv introspection now captures parameter and return annotations.** The
  `inspect.signature` pass previously recorded only each parameter's name /
  kind / has-default, so every third-party parameter became `Type::Unknown`
  and only *arity* was checked. It now also reads `p.annotation` (and the
  return annotation) and maps the unambiguous scalar builtins (`int` /
  `str` / `bool` / `float` / `bytes` / `None`), the nullable forms
  `Optional[X]` / `X | None`, the parametric containers
  `list[X]` / `set[X]` / `frozenset[X]` / `dict[K, V]`, and fixed-arity
  `tuple[...]` (mapped recursively; a container whose element doesn't
  resolve degrades to a permissive `Unknown` rather than `list[Unknown]`)
  to Typhon types. A
  fully-typed pure-Python dependency now gets argument-*type* checking for
  both **free-function** and **constructor** calls through the same
  `tyc::type_mismatch` machinery project functions use — e.g. calling a
  `def fetch(url: str, ...)` with an `int`, or constructing a
  `Client(host: str, port: int)` with `port="oops"`, is rejected at compile
  time. Conservative by design: containers, `Optional`/unions, typing
  constructs, and foreign classes all degrade to a permissive `Unknown`, so
  the change can only *add* true positives, never reject valid code on a
  shape it can't model. Verified against the full 256-file example corpus
  with zero false positives.
- **Constructor arguments are now type-checked (closes an in-project
  soundness hole too).** The non-generic constructor path previously
  validated only arity, so `C(host="x", port="not-an-int")` for a project
  `class C: host: str; port: int` passed `tyc check` and crashed at
  runtime. A new `check_concrete_constructor_args` validates each
  positional / keyword argument against its concrete field type (the
  type-parameter fields of a generic class stay the job of the existing
  `check_generic_constructor_args`, so the two never double-report).
- **Too-many-positional is now caught for zero-field constructors** when the
  shape is authoritative for the constructor — a venv-introspected class
  (`requests.Session(1)` → "expected 0, got 1") or a normal project class
  with a fully-known hierarchy. `plain class` / `class!` (which may carry a
  hand-written `__init__` not reflected in the fields) are deliberately
  exempt, so the change adds no false positives (verified across the
  256-file corpus).
- **Scope / limits** (documented in `docs/diagnostics/missing_argument.md`):
  C-extension callables and typeshed-only (non-inline) annotations remain
  out of scope for the runtime-introspection path — those are covered by the
  new `ty` typeshed integration below.

### Added — `ty` typeshed integration (`[checker] external = "ty"`)

- **`tyc build` can run Astral's `ty` as a second-stage checker over the
  emitted Python**, gated by a new `[checker] external = "ty"` config key
  (`"none"` by default). This is Phase 1 of the long-documented plan in
  `docs/ty-integration.md`, and it's the genuine "works for every library"
  answer: `ty` checks against **typeshed**, so it catches misuse of
  C-extension and stdlib APIs that runtime venv introspection fundamentally
  can't see. Example: `os.path.join(1, 2)` passes `tyc check` (no inline
  signature) but is now caught at build time —
  `error[no-matching-overload]: No overload of function 'join' matches` —
  with the diagnostic re-attributed to the originating `.ty` line via the
  existing `.py.map` source maps. `ty` errors fail the build (CI-gating).
- The shared `run_ty_check` helper is factored out of the `tyc ty` command
  so the standalone command and the build-time hook use one code path.
  `[checker] external-args = [...]` forwards extra flags to `ty` verbatim.
- **`--with-ty` flag on `tyc build` and `tyc check`** — runs the `ty` pass
  for a single invocation without editing `typhon.toml`. `tyc build
  --with-ty` checks the emitted output directly; `tyc check --with-ty`
  (normally emit-free) builds to a throwaway directory first.

### `ty` Phase 2 (embedded) — prototyped and proven feasible, **not shipped**

- The embedded, in-process `ty` checker (calling `ProjectDatabase::check()`
  directly instead of spawning the CLI) was prototyped end-to-end and works.
  It is **not shipped**: it requires a git dependency on `astral-sh/ruff`,
  which the repo's `cargo deny` policy disallows (`[sources] unknown-git =
  "deny"`), and it offers **no capability** the subprocess path lacks (it's a
  perf optimisation). The subprocess integration (`[checker] external = "ty"`
  / `--with-ty`) remains the shipped path.
- **Correction worth recording:** `docs/ty-integration.md` (and earlier
  notes) claimed Phase 2 was blocked until Typhon migrated off its vendored
  Ruff fork, because `ty` consumes `ruff_python_ast`. That's **not** true —
  proven empirically: Typhon's vendored `ruff_python_ast`
  (`0.0.0-typhon-vendor`) and `ty`'s upstream `ruff_python_ast` (`0.0.0`)
  coexist in one dependency graph, kept apart by version. The real gate is
  the git-source supply-chain policy, not a crate-name conflict. Vendoring
  `ty` into `tyc/vendor/` (a path dep, no git source) is the path to shipping
  it later.
- A `.py.map` resolver fix landed alongside and is kept: `load_map_for` now
  resolves **build-relative** `.py` references (`main.py`, `pkg/mod.py`)
  against `<map_dir>/.sourcemaps/<rel>.py.map`, not just the absolute-path /
  adjacent layouts.

### Added — live third-party arg/type diagnostics in the editor (LSP)

- **`tyc lsp` now runs the venv signature introspection**, so a wrong-typed
  or wrong-arity call to an installed third-party dependency shows up as you
  type — not only on `tyc check` / `tyc build`. Previously the LSP checked
  project `.ty`/`.dty` shapes only; `enrich_project_shapes_with_venv` ran
  exclusively on the CLI, so `requests.get(12345)` had no editor squiggle.
- The introspection logic moved to a new shared crate **`tyc-venv`** (it was
  private to the `tyc` binary), now consumed by both the binary and
  `tyc-lsp`. The LSP holds a **persistent per-project `VenvSignatures` cache**
  (mirroring the completion-introspection cache), so a keystroke only shells
  to Python when a genuinely new dependency module appears; the cache
  invalidates itself on `.venv/pyvenv.cfg` mtime change (a `uv sync`). The
  allow-list is refreshed from `typhon.toml` each check via the new
  `allowed_top_level_from_project`. Verified end-to-end by driving the real
  `tyc lsp` server: `api.fetch(12345)` for `fetch(url: str, …)` publishes
  `tyc::type_mismatch: expected str, found int`.

### Added — `unintrospectable-dependency` warning (no more silent misses)

- **A declared dependency that's imported but can't be introspected now
  surfaces a warning** instead of silently skipping its third-party checks —
  the most dangerous failure mode for this feature (a skipped check looked
  identical to a clean pass). Fires when there's no reachable `.venv` /
  `python3`, or the package isn't installed, or it exposes no introspectable
  signatures. New `[strictness] unintrospectable-dependency` knob:
  `"warn"` (default) surfaces it, `"error"` fails the build/check (CI-gating),
  `"off"` restores the old silent behaviour. Per-top-level success tracking
  means a package whose root introspects fine isn't flagged just because one
  submodule failed; an installed, introspectable dependency produces no
  warning.

### Fixed — resolver

- **Dict comprehensions bind tuple-unpack targets.** `{k: v for k, v in
  d.items()}` reported `k` and `v` as unknown names — the dict-comp arm
  only declared single-`Name` targets, while list/set comps used the
  recursive `declare_loop_target`. Dict comps now use the same helper.

### Fixed — second stress round (sub-agent sweep across text/regex, numerics, and the object model)

- **`enum` keyword VM runtime.** `repr(Color.RED)` now renders
  `<Color.RED: 1>` (and enum members inside a container repr the same way,
  not as their backing dataclass fields). Value lookup `Color(2)` and name
  lookup `Color["RED"]` work, returning the singleton member and raising
  `ValueError` / `KeyError` on a miss, matching CPython.
- **`round(int, ndigits)` with negative `ndigits`** rounds to
  tens/hundreds/… (banker's rounding, staying an int) instead of returning
  the integer unchanged. `round(123456, -2)` → `123500`.
- **f-string `:c`** converts an int to its Unicode character (`f"{65:c}"` →
  `"A"`).
- **`!=` derives from a user `__eq__`** when the class defines no `__ne__`,
  instead of falling back to identity.
- **`hash()` dispatches a user `__hash__`** method on instances.
- **`math.prod`** added to the VM `math` shim.
- **`range(...)[i]`** integer indexing (with Python negative indexing).
- **bytes / bytearray are iterable** — `list(b"\x01\x02")` → `[1, 2]`, and
  `sum`/comprehensions over bytes work.
- **`enumerate(iterable, start=N)`** and **`zip(*iterables, strict=True)`**
  honour their keyword arguments (the VM previously rejected both).

### Fixed — emit path

- **Method/attribute access on an integer literal** is now parenthesised:
  `(255).to_bytes(4, "big")` instead of the invalid `255.to_bytes(...)`,
  which CPython rejects as a `SyntaxError` (`255.` parses as a float).
  Applies to attribute access and subscript.

### Fixed — third stress round (`re` module + container repr)

- **`re.sub` / `Pattern.sub` honour a callable replacement.** The VM
  previously stringified the function (`<function repl>`) instead of
  calling it per match. Callable replacements now receive a real Match
  object and their return value is substituted.
- **`re.sub` replacement templates use Python syntax.** `\1`…`\9`,
  `\g<name>`, `\g<N>`, and `\\` are expanded against the captures (the VM
  previously passed the template through Rust's `$1` engine, so `\g<name>`
  came out literally).
- **`re.split` with a capturing group includes the captured text**
  (`re.split(r"(\d)", "a1b2")` → `['a', '1', 'b', '2', '']`), matching
  CPython.
- **`Match.group("name")` and `Match.groupdict()`** resolve named groups
  (the VM previously `int()`-cast every group argument and crashed on a
  name).
- **`re.finditer` / `Pattern.finditer` and `re.subn` / `Pattern.subn`**
  added.
- **`count` / `maxsplit` arguments** on `re.sub` / `re.split` are honoured.
- **Container elements dispatch user `__repr__` / `__str__`.** Objects with
  a custom `__repr__` inside a list/tuple/set/frozenset/dict now render via
  that method (`[<1>, <2>]`) instead of the default dataclass repr. Enum
  members (`[<Color.RED: 1>]`), `Ok`/`Err`, frozensets-of-frozensets, and
  mappingproxy frozen dicts all render correctly; a depth cap guards
  self-referential containers.

### Fixed — fourth stress round (bytes / int / complex stdlib gaps)

- **bytes / bytearray methods** `split`, `rsplit`, `strip`, `lstrip`,
  `rstrip`, `startswith`, `endswith`, `find`, `index`, `replace`, `join`
  added to the VM (previously `AttributeError`).
- **`int.to_bytes(length, byteorder)`** and **`int.bit_count()`** added.
- **`complex("1+2j")`** (and `"3j"`, `"-1"`, `"2-3j"`, `"j"`) string parsing.

### Fixed — power operator complex results

- **A negative base raised to a non-integer power returns a complex number**
  (`(-8) ** (1/3)` → ~`1+1.732j`) instead of `nan`, matching CPython.
- **A complex base raised to a non-negative integer power** is computed by
  repeated multiplication for an exact result (`(1j) ** 2` → `-1+0j`).

### Fixed — checker soundness + a false-reject (third batch)

- **`**kwargs: T` value typing.** Keyword-argument values absorbed by a
  `**kwargs: T` parameter are checked against `T` — `f(a=1, b="oops")` for
  `def f(**kwargs: int)` is now rejected (was unchecked; crashed at
  runtime). `**kwargs: object` accepts anything. (Covers free/local
  functions; method/cross-module `**kwargs` value-checking is still open.)
- **Contravariant `Callable` parameters.** Passing a function with a
  more-general parameter type where a more-specific one is expected is sound
  and now accepted: `Callable[[Animal], str]` is assignable to
  `Callable[[Dog], str]` (params contravariant via the inheritance-aware
  `is_assignable`); the unsound reverse stays rejected.
- **`unsafe:` value leak into an annotated assignment.** A value produced
  inside an `unsafe:` block that flows into a concrete-typed `let n: T = v`
  now fires `tyc::unsafe_value_leak` (previously only audited on `return`);
  re-asserting inside the block or targeting `object`/`T?` is exempt.

### Fixed — VM nested-package `pub *`

- **`pub *` in a package's `__init__.ty` is aggregated under `tyc run`.**
  The VM module loader now scans the package directory when an `__init__.ty`
  contains `pub *`, loading each sibling module / sub-package and
  re-exporting its `pub` names — so `from pkg import f` resolves in the VM
  the same way it does after `tyc build` (previously `AttributeError:
  module 'pkg' has no attribute 'f'`).

### Fixed — type-checker soundness (union member access)

- **Operations on a union type must be valid for every member.** The
  checker previously accepted an operation if *any* member supported it, so
  `let x: int | str = "two"; x + 1` type-checked clean then crashed with a
  runtime `TypeError`. Now binary operators, attribute/method access, and
  `len()` are checked against *all* members of a union (`x + 1` on `int |
  str` → `operator_type_mismatch`; `x.upper()` → `attribute_not_found` on
  `int`). Conservative: members that are user classes with unknown
  hierarchy, `__getattr__`, `Unknown`, `TypeVar`, or `Any` stay permissive,
  so legitimate duck-typed unions and narrowed uses still pass. Corpus
  unchanged; no example needed a fix.

### Fixed — VM exception args

- **Builtin exceptions keep all constructor arguments.** `Value::Exception`
  gained an `args` tuple, so `ValueError("a", "b", 3).args` is `('a', 'b',
  3)` (was `('a',)`), `str(e)` of a multi-arg exception is the tuple form,
  and the args survive through `raise` into an `except ... as e` handler.
  `e.__cause__` / `e.__context__` read as `None` (exception chaining storage
  is still a backlog item).

### Fixed — comptime operators

- **`comptime` now supports `//`, `%`, and `**`** (floor-division, modulo,
  power), matching the documented arithmetic surface and Python semantics
  (`-7 // 2 == -4`, `-7 % 2 == 1`, `2 ** 10 == 1024`, `2 ** -2 == 0.25`).
  Division/modulo by a zero divisor produces a clean comptime error.

### Fixed — VM property setters and `dir`

- **`@<prop>.setter`** is honoured: `obj.prop = v` invokes the setter
  (previously the `@c.setter` decorator crashed with `NameError` and the
  assignment bypassed the setter).
- **`dir(x)`** returns the sorted attribute names of an instance / class /
  module (was `NameError`).

### Fixed — VM parity with the checker false-reject fixes

- **`__getattr__` resolves missing attributes** under `tyc run` (was
  `AttributeError`), matching the build path.
- **A walrus binding in a comprehension leaks its last value** to the
  enclosing scope under `tyc run` (was `NameError`).

### Fixed — type-checker false-rejects (valid code wrongly rejected)

- **`plain class` and `class!` may define a hand-written `__init__`.**
  `tyc::manual_init` no longer fires on them (a `plain class` generates no
  constructor and `class!` preserves the user's `__init__` verbatim). It
  still fires for a normal `class` / `model`. (The `class!` half also
  needed the resolver to recognise raw classes in the `tyc check` path,
  which doesn't thread the `ClassKind::Raw` line markers — `class!` names
  are now scraped from source like `plain class` names.)
- **`tyc::class_attr_shadows_slot` no longer fires on `plain class`** (its
  class-level defaults are real class attributes, not slot descriptors).
- **A class defining `__getattr__`** suppresses `tyc::attribute_not_found`
  for attribute access on its instances.
- **Attributes assigned via `self.x = ...` in a method** are tracked, so
  `self.d[k]` no longer false-positives `attribute_not_found`.
- **A walrus binding in a comprehension** (`[y for x in xs if (y := f(x))]`)
  leaks to the enclosing scope, matching Python.

### Fixed — sixth stress round (VM data model, part 2)

- **List slice assignment** `xs[1:3] = [...]` (contiguous and extended-step).
- **`__index__`** is honoured for subscripting (`xs[idx]` / `xs[idx] = v`).
- **`del obj[key]`** dispatches `__delitem__`; **`del obj.attr`** removes an
  instance attribute.
- **`@cached_property`** is invoked on read (returns the value, not the bound
  method); class-level `lazy let` rides on the same path.
- **`__iter__` returning `self`** (the standard iterator idiom) no longer
  stack-overflows — the VM drives `__next__` to `StopIteration`.
- **Function `__name__` / `__qualname__`** (also on natives and bound
  methods).
- **Set literal star-unpack** `{*xs, 9}`.
- **Bare-class `raise`** (`raise StopIteration` / `raise SomeError`)
  instantiates the exception, matching Python.

### Fixed — sixth stress round (VM exception model)

- **Builtin exception hierarchy catching.** `except ArithmeticError` now
  catches `ZeroDivisionError`, `except LookupError` catches
  `KeyError`/`IndexError`, `except OSError` catches `FileNotFoundError`,
  etc. Previously only the exact type and `Exception`/`BaseException`
  matched, so intermediate-base handlers silently let exceptions escape.
- **Bare `raise` (re-raise)** inside an `except` handler re-raises the
  active exception instead of erroring with "No active exception".
- **An exception raised in a `finally` replaces the in-flight exception**
  (CPython semantics) instead of being discarded.
- **`type(exc).__name__`** reports the concrete kind (`TypeError`) instead
  of the generic `Exception`.
- **`__exit__` receives `(exc_type, exc_value, None)`** when the `with`
  body raised, and **a truthy return value suppresses the exception**.
  Previously `__exit__` always got `(None, None, None)` and could not
  suppress.

### Fixed — fifth stress round (VM data model + a second soundness hole)

VM:
- **`__bool__` / `__len__` are consulted for truthiness** (`bool(x)`, `if x`,
  `and`/`or`/`not`, ternaries, comprehension filters, match guards, `assert`).
  An empty `__len__` object is now falsy.
- **Dynamic-attribute builtins** `getattr` (with default), `setattr`,
  `hasattr`, `delattr`, `vars` added to the VM.
- **Class-level attribute access** — `ClassVar[T]` fields and `plain class`
  constants are readable as `Cls.X` and `instance.X` (the VM was treating
  every annotated class-body assignment as an instance slot). Root cause:
  the VM's `run` path never passed the preprocessor's plain/raw/frozen
  class-kind markers to the desugarer, so `plain class` / `class!` /
  `class … frozen` were desugared differently from `tyc build`; the markers
  are now threaded through.

Type-checker soundness:
- **Optional element types are preserved on extraction.** Indexing,
  iterating, or comprehending a `list[T?]` / `dict[K, V?]` / set yields `T?`,
  not `T`. Previously every extraction dropped the `?`, so `let a: Animal =
  src[0]` (with `src: list[Animal?]`) type-checked clean then crashed with
  `AttributeError: 'NoneType'`. Comprehensions are now type-checked at all
  (they previously inferred `Unknown`). The fix surfaced and corrected one
  genuine latent unsoundness in `examples/68-json-rpc-builder`.

### Fixed — type-checker soundness

- **Generic-class constructor arguments are validated against the bound
  type parameters.** `let b: Box[int] = Box("hello")` (and `Box[int](...)`
  / `Pair[int, str](...)`) now raise `tyc::type_mismatch` instead of
  type-checking clean and crashing at runtime. The check is conservative —
  pure inference (`Box(5)`), `int`→`float` widening, and `None` into a `T?`
  field still pass.

### Known gaps documented (not yet fixed)

- `@contextmanager` generators used as `with` context managers still raise
  `NotImplementedError` under `tyc run` (the tree-walking VM materialises
  generators eagerly). Use `tyc build` + CPython.
- The VM's seeded `random` does not reproduce CPython's Mersenne Twister.
- `set`/`dict` keying still uses a structural hash key rather than a user
  `__hash__`/`__eq__` (the `hash()` builtin and `!=` were fixed; the keying
  path is deeper). Regex backreferences-in-pattern (`(\w+) \1`) and
  lookaround remain unsupported by the underlying finite-automaton engine.
  Counter/defaultdict/OrderedDict repr, `Path.parent` returning a `Path`,
  `float.hex()`, and `decimal`/`fractions`/`statistics` modules remain on
  the backlog.

## 0.11.0 — 2026-06-04

VM parity sweep + `enum` keyword. A fresh adversarial stress round
against v0.10.0 surfaced 22 findings — almost entirely in the VM, with
a handful of type-checker coherence gaps. The language frontend and
the build path (`tyc build` → CPython) held up; this release closes
every finding from the round, lands the proposed `enum` keyword as a
first-class declaration form, and introduces two new VM value kinds
(`Value::Complex` for native complex arithmetic, and a dict-view kind
behind `dict.keys()` / `.values()` / `.items()` so they repr and
behave like CPython).

### Added — `enum` keyword

`enum Name:` is now a first-class declaration that sugars over
`enum.Enum`, mirroring how `model` sugars over `pydantic.BaseModel`
and `class!` sugars over a framework base. Bare members (`CIRCLE`,
`SQUARE`) auto-fill with `enum.auto()`; explicit `RED = 1` is
preserved. `tyc-syntax` preprocesses the header / body, `tyc-emit`
injects `import enum` when an `enum.*` base is present, `tyc-resolve`
adds `enum` to the builtin prelude so `tyc check` accepts it before
the import is injected, and `tyc fmt` round-trips the header and
members. Mixed forms work too — `enum.auto()` continues numbering
from the last explicit value.

```typhon
enum Shape:
    CIRCLE
    SQUARE
    TRIANGLE

enum Color:
    RED = 1
    GREEN = 2
    BLUE = 4
```

### Added — VM `Value::Complex` and dict-view kind

- **`Value::Complex(f64, f64)`** is a real VM value. `complex(re, im)`
  / `complex("1+2j")` construct it, the `complex` literal form parses,
  arithmetic between complex / int / float promotes correctly, the
  reflected dunders (`__radd__` / `__rmul__` / …) dispatch, and
  `complex` instances are hashable for dict / set keys. Previously
  every complex-typed expression panicked at the VM boundary.
- **`Value::DictView`** backs the iterators returned by `dict.keys()`
  / `.values()` / `.items()`. They repr as
  `dict_keys([...])` / `dict_values([...])` / `dict_items([...])`,
  iterate, support `len`, are membership-testable with `in`, and are
  re-iterable. Previously each method materialised a fresh list, so
  `d.keys() == d.keys()` was identity-false and `repr(d.keys())`
  printed `[...]`.

### Added — VM enum runtime and bare-`super()` rewrite

- **`enum` module is native.** `enum.Enum` / `enum.auto()` resolve under
  `tyc run`, members materialise on first class-body execution, the
  class iterates in declaration order, and `ClassName.MEMBER` repr
  matches CPython (`<Shape.CIRCLE: 1>`). A `loading_modules` recursion
  guard prevents the enum module from re-entering itself.
- **Bare `super()` rewritten to two-arg form** in `tyc-desugar`. The
  zero-arg `super()` crashed emitted code under
  `@dataclass(slots=True)` (which orphans the `__class__` cell). A new
  `rewrite_bare_super` pass runs after the impl / extend merge and
  rewrites every bare `super()` inside a method body to the explicit
  `super(EnclosingClass, self)` form, stopping at nested def / class
  scopes. Explicit `super(X, y)` calls are left untouched.
- **`__call__` dispatch on callable instances.** `inst(args)` for a
  class that defines `__call__` now dispatches the dunder instead of
  raising `TypeError: object is not callable`.
- **`__post_init__` invoked after auto-generated construction.** The
  build path already did this; the VM was silently skipping it, so a
  dataclass with `__post_init__` produced a half-initialised instance.
- **Multi-level inheritance field accumulation.** A subclass three or
  more levels deep now accumulates fields from every ancestor in MRO
  order, not just the immediate base.

### Added — instance operator dunders + subscript `__missing__`

Builds on v0.10.0's dunder dispatch. The generic operator handler in
`binop` now reaches every numeric / bitwise / matmul slot
(`__add__` / `__sub__` / `__mul__` / `__truediv__` / `__floordiv__` /
`__mod__` / `__pow__` / `__matmul__` / `__lshift__` / `__rshift__` /
`__and__` / `__or__` / `__xor__`) with reflected fallback, on every
instance pair regardless of class — this is what unblocks
`pathlib._Path / "subdir"` and the new datetime arithmetic shims.
Subscript `__missing__` fires when `__getitem__` doesn't find the key,
which is what backs the new `defaultdict` factory.

### Added — VM stdlib expansion

- **`collections.defaultdict(factory)`** materialises as an
  Instance-backed dict that consults `factory()` on missing-key
  access (`subscript __missing__`). `dd[k] += 1` works.
- **`datetime` module shim.** `datetime.datetime(y, mo, d, ...)`,
  `.now()`, `.fromisoformat(...)`, `.isoformat()`, `+ timedelta`,
  comparisons, and `timedelta(seconds=...)` arithmetic resolve
  natively. Naïve / UTC only; tz-aware arithmetic still needs
  `--compile`.
- **`pathlib` module shim.** `Path("a") / "b"` joins via `__truediv__`,
  `.parent` / `.name` / `.stem` / `.suffix` / `.suffixes` / `.parts`
  resolve, and `str(Path(...))` / `repr(Path(...))` match CPython.
  `.read_text` / `.write_text` are already wired through v0.10.0's
  `open()` plumbing.
- **`bytes` methods** (`decode` / `hex` / `fromhex` / `count` / `find` /
  `rfind` / `startswith` / `endswith` / `split` / `strip`),
  **`itertools.groupby`**
  honours `key=` instead of grouping by identity, **`re.Match.group(n)` /
  `.groups()` / `.groupdict()`** return the real capture groups (the
  prior shim returned the whole match for every group index),
  **`str.split(maxsplit=…)`** as a pure-keyword arg, **f-string
  `{x=}`** debug conversion renders `x=<repr>`, and **`str %`** /
  **f-string `%`** percent format types work at runtime.
- **`builtins.round`** uses banker's rounding (half-to-even) instead of
  away-from-zero, matching CPython.

### Changed — VM value semantics align with CPython

These were silent-wrong outcomes under v0.10.0 (`tyc run` returned a
different value than `tyc build` + `python`) and are now fixed in
`tyc-vm/src/value.rs`. Programs that relied on the old behaviour will
see different (correct) results:

- **Dataclass instance equality is value-based** (same class + all
  fields equal recursively) instead of identity. The class identity
  test uses the underlying `Class` pointer, not the name, so distinct
  same-named classes from different modules no longer collide.
- **Dataclass instance `repr` is `Name(field=value, ...)`** in declared
  field order. Previously printed `<Name instance>` for every
  dataclass.
- **Dataclass instances are hashable** via a new `HashKey::Instance`
  variant (class identity + fields sorted by name), so frozen-dataclass
  dict / set keys work and equal-field instances collide as keys.
- **Set / frozenset equality is order-independent**, matching CPython
  set semantics. Two sets with the same members in different insertion
  order now compare equal.
- **Set / frozenset repr** sorts elements by a canonical key for
  deterministic, CPython-matching order.
- **Float `repr`** matches CPython's `repr(float)` — shortest
  round-tripping form, scientific notation for exp < -4 or ≥ 16
  (`e+NN` / `e-NN`, ≥ 2 exponent digits), `-0.0` preserved.

### Added — type checker tightening

- **`None` flows into `object`.** A `None`-valued expression now
  satisfies an `object` parameter / annotation, matching CPython's
  `None: object` invariant. The prior rejection blocked common
  defaulting patterns (`def log(msg: object = None)`).
- **`str %` is type-checked.** `"x=%d" % "not an int"` now fires
  `tyc::operator_type_mismatch` against the inferred conversion-spec
  shape, matching the existing `str.format` check.
- **Builtin scalar attribute / subscript / iteration validated.**
  `(5).items()`, `5["a"]`, and `for x in 5:` now fire the appropriate
  diagnostic at check time instead of crashing at run time.

### Fixed — instance equality / hashing key on class identity

`__eq__` / `__hash__` on user instances were keyed on the class name,
so two distinct classes named `Point` from different modules compared
equal and shared a hash bucket. The dispatch now keys on the
underlying `Class` pointer (identity-equal in the VM's interned class
registry), so cross-module same-name collisions are gone.

### Fixed — generated `typhon.toml` includes `allow-secret-comptime`

`tyc init` now seeds `allow-secret-comptime = false` in the generated
`typhon.toml` under `[strict]`, matching the documented default and
making the escape hatch self-discoverable. Existing projects are
unaffected; the knob continues to be opt-in by setting it to `true`.

### Fixed — docs-site high-contrast / card-hover a11y

High-contrast mode lifts card text contrast, normalises focus rings on
hover transitions, and keeps card interactivity legible when
`prefers-contrast: more` is set. Card-hover animations remain wrapped
by `prefers-reduced-motion`.

### Fixed — venv batch introspection retries transient fork failures

`tyc-introspect` retries up to 4 times with exponential backoff
(2s / 4s / 8s / 16s) when a venv-introspection subprocess fails with a
transient fork / clone error, so CI environments with constrained
process tables don't see spurious `tyc::attribute_not_found` cascades.

### Changed — emit hot path: decorator matching + complex emission

`tyc-desugar`'s decorator-list matching no longer heap-allocates per
visited decorator, and complex-number literal emission in `tyc-emit`
shares a stack buffer with the `itoa` / `ryu` paths. Continues the
v0.10.0 emit hot-path effort.

### Known limitations carried forward from v0.10.0

- Lazy / unbounded VM generators still need `tyc build` (eager
  collection caps at 1M items).
- `@contextmanager` generators inside `with` blocks still need
  `tyc build` under the VM.
- Nested-model `pydantic.model_validate` is not type-directed in the
  VM; deeply-nested models still need `tyc build`.
- Tz-aware `datetime` arithmetic still needs `tyc build` (the native
  shim is naïve / UTC only).
- Preprocess line-number leakage (B15) for `impl Alias:` distribution
  over sealed unions — unchanged from v0.9.0.

## 0.10.0 — 2026-06-01

VM completeness release. Stress-testing the tree-walking VM past v0.9.2
surfaced a large batch of correctness and coverage gaps that prevented
`tyc run` from being a drop-in replacement for `tyc build && python`
on real-world programs. This release closes the biggest of them — the
VM now dispatches dunders / rich-comparisons on user classes, runs
finite generators, models `type(x)` as a real type object, and ships
the long-tail of missing string / set / math / json / time builtins.
The type checker also picks up three exhaustiveness / augmented-assign
false-positive fixes, and `tyc-emit` shaves heap allocations out of
the literal-emission hot path.

### Added — VM dunder dispatch and rich comparisons

- **Operator overloading on instances.** `__add__` / `__sub__` /
  `__mul__` / `__truediv__` / `__floordiv__` / `__mod__` / `__pow__` /
  `__matmul__` / `__lshift__` / `__rshift__` / `__and__` / `__or__` /
  `__xor__` plus the reflected `__radd__` / `__rmul__` / … forms now
  dispatch on user-class instances. Previously `a + b` for two user
  instances raised `TypeError: unsupported operand type`.
- **Rich comparisons on instances.** `__eq__` / `__ne__` / `__lt__` /
  `__le__` / `__gt__` / `__ge__` now dispatch. The `in` operator and
  `list.index` / `list.count` / `list.remove` / list-tuple membership
  now use `__eq__`-aware comparison instead of identity, so they work
  on instances that define equality.
- **`__str__` / `__repr__`** honoured by `print` / `str` / `repr` /
  f-strings. `__len__` / `__getitem__` / `__contains__` dispatch on
  the matching builtin call.
- **`@property` getters** invoked on attribute read. **`@classmethod`**
  binds the class as `cls`. Both inherited through bases.
- **Inherited `@property` / `@classmethod` markers cleared on
  override.** A subclass plain method shadowing an inherited property
  no longer carries the descriptor marker.

### Added — VM generator support

- **`yield` / `yield from` finally work under `tyc run`.** A tree-walk
  can't suspend a frame and `Rc` values aren't `Send` (ruling out
  thread-based coroutines), so generators are eagerly materialised: a
  yield-bearing function runs to completion with each yielded value
  pushed onto a per-call buffer, and the call returns an iterator over
  the collected values. Detected via `body_is_generator` (scans for
  `yield` without crossing nested function / lambda boundaries); nested
  / recursive generators get a stack of buffers.
- **`GENERATOR_CAP = 1_000_000`** bounds the worst case (`while True:
  yield`) to a clear `RuntimeError` instead of an unbounded hang. Lazy /
  unbounded generators still need `tyc build`.
- **`@contextmanager` generators in `with`** get a clear "use `tyc
  build`" message — eager collection runs setup + teardown at call
  time, so the `with` body can't run between them.

### Added — VM `type(x)` as a real type object

`type(x)` previously returned a plain string, so two common idioms
were silently broken: `type(x).__name__` produced a bound method, and
`type(x) == int` was always false. It now returns a type object —
user instances map to their declaring `Class`, builtins map to a
lightweight named class object. `type(x).__name__` / `Cls.__name__`
resolve to the type name, `str(type(x))` renders `<class 'int'>`, and
equality holds across all the expected cases: `type(a) == type(b)`,
`type(inst) == SomeClass`, `type(5) == int` (matched against the
builtin constructor's name), `type(5) == type(6)`. Native==Native
stays identity-only so distinct bound methods don't compare equal,
and builtin type objects are cached singletons so `Class` equality is
identity-based (no cross-module same-name collisions).

### Added — VM pydantic `model_validate` / `model_dump`

`Model.model_validate(mapping)` constructs a model instance from a
dict (maps fields to constructor kwargs); `inst.model_dump()` returns
a dict of the instance's fields in declaration order, and
`model_dump_json()` the JSON form. Flat `model` classes are now
usable under `tyc run`. Nested-model validation is not type-directed
yet — a nested dict stays a dict; use `tyc build` for deeply-nested
models.

### Added — VM keyword args to builtin methods

- `max()` / `min()` accept `key=` and `default=` keyword arguments.
- `list.sort()` accepts `reverse=` and `key=` (previously rejected all
  kwargs). `list.sort` honours user `__lt__` / `__eq__` via
  `interp.value_cmp` for both the keyed and unkeyed paths.
- Implemented via a kwargs sentinel (`make_kwargs_sentinel` /
  `split_kwargs`) since native fns have no kwargs slot. The dispatcher
  unpacks the trailing sentinel before invoking the builtin.

### Added — VM missing string / set / numeric / time / json builtins

- **String methods**: `index`, `rindex`, `center`, `ljust`, `rjust`,
  `zfill`, `rsplit`, `partition`, `rpartition`, `removeprefix`,
  `removesuffix`, `casefold`, `isnumeric`, `istitle`, `expandtabs`.
  `strip` / `lstrip` / `rstrip` now honour their `chars` argument
  (previously ignored, silent wrong results). `str.format`
  stringifies values through `str_of` so a user `__str__` is
  honoured.
- **Set algebra**: `union`, `intersection`, `difference`,
  `symmetric_difference`, `issubset`, `issuperset`, `isdisjoint`,
  `update`. `frozenset.union` / `intersection` / `difference` /
  `symmetric_difference` preserve the frozen sentinel; `update` is
  rejected on frozensets.
- **`dict(other_dict)`** shallow-copies; **`dict(**kwargs)`** keyword
  form works.
- **`divmod`**: raises `ZeroDivisionError` (not `ValueError`) for int
  and float divide-by-zero, with CPython messages.
- **`pow`**: 2-arg and 3-arg modular pow.
- **`format`** / **`ascii`** added. **`ascii`** also registered as a
  known builtin in the resolver.
- **`int(str, base)`**: now supports `base=0` (autodetect 0x / 0o /
  0b radix) and validates the base is an integer.
- **`math.gcd` / `lcm` / `factorial` / `isqrt` / `comb` / `perm`**
  reject non-integer args (a float cannot be interpreted as an
  integer).
- **`len(obj)`**: a user `__len__` must return a non-negative int.
- **`str_of` / `repr_of`**: a user `__str__` / `__repr__` returning a
  non-`str` raises `TypeError`.
- **`json.dumps(indent=…)`** pretty-prints.
- **`time.perf_counter`** and **`process_time`** added.
  **`time.monotonic`** fixed (was always returning ~0).
- **Unbound builtin-type methods** (e.g. `str.lower(x)`) work,
  unblocking the documented pipe idiom that lowers to
  `str.lower(x)`.

### Fixed — VM iterator-adapter panic

`enumerate` / `zip` / `map` / `filter` panicked with
`RefCell already borrowed` the instant they were iterated. The match
scrutinee in `iter_next` held a borrow that the matched arm
re-borrowed when it recursed. The borrow is now released before the
recursive call.

### Fixed — `Path.read_text` / `write_text` / `open` tolerate
`encoding=` / `errors=`

The VM doesn't model these kwargs (it is always UTF-8), but the
builtins now accept and ignore them instead of rejecting the call.

### Added — type checker exhaustiveness fixes

- **Exhaustive `match` on `bool` and string-literal unions.**
  `match b: case True / case False` and `match s` over a `type C =
  "a" | "b"` union matched on every member no longer false-positive
  `tyc::missing_return`. `cases_cover_type` now certifies a `bool`
  subject when both arms (or a wildcard) are present, and a `LitStr`
  subject when a guardless `case "literal":` covers each variant.
- **Irrefutable fixed-arity tuple patterns recognised as exhaustive.**
  A `match` on a `tuple[int, int]` ending in a guardless
  `case (x, y):` covers every inhabitant (the tuple's length is
  statically known), but the missing-return analysis only certified
  sequence coverage when a tail-star arm was present. Variadic
  `tuple[int, ...]` is intentionally excluded; non-exhaustive tuples,
  fixed-length patterns over variable-length lists, and wrong-arity
  patterns all still fire.
- **Augmented assignment on scalar targets is type-checked.**
  `s += 5` (str + int) now fires `operator_type_mismatch`, matching
  the existing check on `s = s + 5`. Restricted to scalars
  (int / float / bool / str / bytes); mutable containers keep their
  looser in-place semantics (`list += any_iterable`) to avoid false
  positives.

### Fixed — strictness knob plumbing

`allow_secret_comptime` was documented in `typhon.toml` strictness
config but never threaded into `analyse_secret_literal_bindings`, so
the user-facing escape hatch had no effect. Now wired through.

### Changed — emit hot path heap allocations

- Integer and float literal emission switched from heap-allocating
  `.to_string()` / `format!` to stack-allocated `itoa` / `ryu`
  buffers in the AST emitter. Falls back to `to_string()` for
  bigints outside `i64`/`u64`; handles `NaN` / `inf` explicitly
  since `ryu` doesn't support them.
- Char and string emission in `tyc-emit` no longer allocates per
  call.

### Fixed — examples / docs-site polish

- `examples/apps/13-vector-db` dropped invalid explicit type args on
  function calls (`new_collection[int](...)`) that the v0.9.0 checker
  correctly rejects. The brackets are gone; inference deduces the
  type parameter at every call site.
- docs-site a11y: alt text on the hero image, `<kbd>` tags around
  keyboard shortcuts, `:focus-visible` extended to inputs and
  textareas, `prefers-reduced-motion` wrapping on card hover
  animations, table-row hover background transition.

### Known limitations carried forward

- Lazy / unbounded VM generators still need `tyc build` (eager
  collection caps at 1M items).
- `@contextmanager` generators inside `with` blocks still need
  `tyc build` under the VM (eager collection runs setup + teardown
  at call time).
- Nested-model `pydantic.model_validate` is not type-directed in the
  VM; deeply-nested models still need `tyc build`.
- Preprocess line-number leakage (B15) for `impl Alias:` distribution
  over sealed unions — unchanged from v0.9.0.

## 0.9.2 — 2026-05-27

Bugfix point release closing a cross-module regression surfaced by the
v0.9.1 MNIST CNN stress sweep. No language, runtime, or
diagnostic-surface changes.

### Fixed — cross-module `tyc::attribute_not_found` on `class!` subclasses of foreign bases

A `class! Sub(Foreign):` (e.g. `class! HttpError(Exception): code: int`)
declared in one module and imported by another false-positived
`tyc::attribute_not_found` on every inherited / framework-provided
attribute access. The same access pattern stayed (correctly) lenient
in-module — the regression only fired across module boundaries.

The root cause was in cross-module shape ingestion. When the consumer
checker seeds external `class_shapes` (from a sibling module's
`extract_module_shapes`), it never seeded the companion `class_parents`
map. The four hierarchy walkers
(`class_hierarchy_fully_known`, `find_method`, `find_field`,
`class_inherits_from`) all consult `class_parents` to traverse the
inheritance chain — so for an imported `HttpError`, the walks stopped
at `HttpError` itself, never saw `Exception`, and
`class_hierarchy_fully_known` reported the chain as "fully known". The
v0.8.0 attribute-existence check then fired on every attribute that
HttpError didn't declare directly, even though the v0.7.1 foreign-base
suppression should have kicked in.

The fix seeds `class_parents` from `InterfaceShape.bases` during the
external-shape ingestion (`check_module_with_imports`).
`InterfaceShape.bases` already crosses the module boundary as part of
the shape; it just wasn't being unpacked into the parents map. The
in-module `collect_classes_and_functions` pass continues to insert into
`class_parents` later in the same checker run; we use
`entry(..).or_insert_with(..)` so a local declaration of the same name
still wins on collision.

After the fix, the four hierarchy walks see the same parent chain for
cross-module classes that they see for in-module ones, the v0.7.1
foreign-base leniency kicks in correctly, and downstream code that
imports `class! HttpError(Exception):` (or `class! M(nn.Module):`,
`class! ParseError(ValueError):`, etc.) no longer needs an `unsafe:`
shim or per-attribute annotations. Two new tests guard against
regression: one asserts the cross-module bogus access is lenient when
the hierarchy includes a foreign base, the other asserts a bogus
access on a fully-known cross-module class (no foreign base) still
fires the diagnostic — guarding against an over-broad fix.

## 0.9.1 — 2026-05-27

Bugfix point release closing two issues surfaced by a v0.9.0 stress
sweep on a six-package PyTorch MNIST CNN. No language, runtime, or
diagnostic-surface changes.

### Fixed — `tyc fmt` round-trip corruption

`tyc fmt` could rewrite valid source into invalid (and in one case
silently-empty) output. Four corruption modes all stemmed from the
same underlying gap — the formatter ran the desugar pipeline's
preprocess pass (which expands `impl Alias:` for sealed-union aliases
into one synthetic `impl Variant:` block per variant, and which
records line-shifting strips against the pre-normalisation source),
then applied PEP-8 blank-line insertion that shifted line indices the
postprocess restoration step still used unshifted:

- **`impl <SealedUnionAlias>:` no longer mangled.** A header like
  `impl AppError:` on a `type AppError = ParseErr | NetErr` no longer
  expands into `class __typhon_impl_AppError(object):` plus a bare
  leftover `impl` token on a separate line. The formatter now runs a
  format-mode preprocess that skips the sealed-union expansion
  entirely (the new `PreprocessOptions.expand_impl_sealed_unions =
  false`); the desugar / build / check pipelines all continue to
  expand as before.
- **`frozen` modifier preserved.** `class P frozen:` used to come back
  as `class P:` after formatting, silently downgrading a frozen
  dataclass to a mutable one. The preprocessor now records a
  `TyphonKeyword::Frozen` entry on every strip site and the
  postprocess pass reinserts the modifier after the class name and
  type-param list, before the base list and trailing colon.
- **`pub *` no longer deleted.** A standalone `pub *` line in
  `__init__.ty` used to come back as an empty file (so every package
  facade silently became empty and downstream callers got
  confusing `ImportError: cannot import name …` failures). The
  preprocessor now records a `TyphonKeyword::PubStar` entry and the
  postprocess pass restores the marker.
- **Multi-line kwarg `=` no longer respaced.** `P(a=3, b=4)` on a
  single line was kept tight (PEP-8), but the same call spread over
  multiple lines came back as `a = 3, b = 4` because the per-line
  spacing rule tracked paren depth line-locally. The depth now carries
  across lines via the file-level normaliser, so continuation lines
  inside an open `(` keep kwargs tight too.
- **Line-index translation across blank-line insertion.** PEP-8
  blank-line insertion before top-level `def`/`class` shifted line
  indices for the entire tail of the file; keyword restoration then
  applied `Pub`/`Frozen`/`Impl`/etc. prefixes to the wrong lines.
  `normalise_whitespace_with_map` now returns an input→output line
  index map that the formatter applies to the stripped / optional /
  lazy-import lists before postprocess walks them.

### Fixed — sealed-union flow through `pub *` package facades

An exhaustive `match` over a sealed-union variant imported through a
`pub *` package facade fired `tyc::missing_return` on every arm-
terminated function, even though the exhaustiveness checker accepted
the match (no `tyc::non_exhaustive_match` diagnostic). The two
analyses disagreed because the consumer's `sealed_unions` registry
was seeded from the package's `__init__.ty` shape — which is empty
when the file is just `pub *` — and the reachability/return pass
falls through to "not a sealed union" when the variant list is
missing.

The fix aggregates the `pub *` facade at the shape-map level, mirroring
what the build pipeline already does at the source level (synthesising
`from .sibling import …` lines). A new
`tyc::commands::util::aggregate_pub_star_shapes` runs after
`collect_project_shapes` and before the per-file check loop in both
`tyc check` and `tyc build`: every `__init__.ty` carrying `pub *`
gets its sibling modules' `pub` surface and every direct sub-package's
effective surface merged into the package's shape entry. Sub-packages
are processed deepest-first so a parent picks up the already-
aggregated child surface. First-write-wins on collisions (cross-
sibling name conflicts are independently surfaced by
`detect_pub_star_diagnostics`).

After this fix, `from <pkg> import Command` in a downstream module
sees `Command` as the same sealed-union alias the source module
declared, with the same variant list — so an exhaustive arm-
terminated match on `Command` no longer surfaces `missing_return`,
and the user no longer needs the `mut result: …` sentinel
workaround.

## 0.9.0 — 2026-05-27

Stress-test cleanup release. v0.8.1 stress testing surfaced 36
findings spanning the type checker, VM, parser, lowering passes,
diagnostics, and CLI. This release closes 32 of them — the VM is now
usable as the daily-driver runner the docs always advertised, the
type checker plugs several silent-correctness gaps, and the
diagnostic surface gets a polish pass.

### Added — VM coverage (closing the gap between `tyc run` and `tyc build && python build/main.py`)

- **`Result` combinators** (`.map` / `.map_err` / `.and_then` /
  `.or_else`) now work in the VM via bound `NativeFn` wrappers that
  capture the receiver. Previously a typecheck-clean program crashed
  at run-time with `AttributeError: Ok has no attribute 'and_then'`.
- **`open()` honours write / append / binary modes.** `open(p, "w")` /
  `open(p, "a")` / `open(p, "wb")` / `open(p, "r+")` and friends now
  all work. `with`-blocks honour `__enter__` / `__exit__` on the
  resulting file. `json.load` / `json.dump` ride on top.
- **Match against built-in class patterns.** `match x: case str() as
  s:` / `case int() as n:` / etc. now matches; the exhaustiveness
  pass also recognises `case None:` + `case str() as s:` as covering
  `str?`.
- **`frozenset(...)` is hashable as a dict key** (new
  `HashKey::FrozenSet` variant with insertion-order-independent
  hashing).
- **f-string `_` thousands separator** emits the same way `,` does.
- **`bytes` repr matches CPython.** `b'hi'` (single quotes by
  default), `b"with 'embedded'"` fallback, `\xNN` for non-printable.
- **Native shims for `collections.deque`, `heapq`, `contextlib`,
  `pydantic`.** Graph / queue / heap algorithms, `@contextmanager`
  identity decorators, and `model` class declarations all run
  cleanly. `deque` rides on `Value::List` via new
  `popleft` / `appendleft` / `extendleft` / `rotate` list methods.
- **`@property` / `@classmethod` / `@staticmethod` / `super()`
  builtins** are present as identity-ish stubs so decorated methods
  no longer crash on import.
- **`lazy import np = numpy`** uses the simpler `import M as N`
  rewrite in VM mode (the descriptor-based proxy class the build
  path emits has nothing to bind against in a tree-walking VM).
- **Multi-file projects run under both `tyc run` modes.** The VM
  loads sibling `.ty` modules from the project source root, honours
  relative imports (`from .repo import x`), and caches each module's
  bindings as a `Value::Module`. `tyc run --compile` now spawns
  `python -m <pkg>.main` instead of `python build/main.py` so
  relative imports in the entry point resolve correctly.
- **`dataclasses.field(default_factory=list)` actually invokes the
  factory per instance.** The mutable-default rewrite no longer
  shares one list across every instance.
- **`class!` synthesised `__init__` runs.** `except HttpError as e:
  print(e.code)` works against `class! HttpError(Exception): code:
  int; message: str` — the handler binds the user Instance, and
  exception-type matching walks the MRO.
- **`freeze let CFG = {...}` actually freezes** (list → tuple, dict →
  mappingproxy-tagged dict, recursive). Mutators on a frozen dict
  raise the same `TypeError` CPython's MappingProxy does.
- **`comptime let X = ...` inlines in the VM** via the substitution
  pass shared with `tyc build`. `comptime let PORT = int(env(...))`
  no longer crashes with `NameError: env is not defined`.
- **Typed tuple unpack `let (a: int, b: str) = pair()` parses in the
  VM** (parity with `tyc check`).

### Added — type checker

- **Sequence / Iterable / Iterator / Collection / Container /
  Reversible are covariant for built-in containers.** `list[Dog]`
  flows into `Sequence[Animal]` when `Dog` inherits `Animal`.
  Mapping / MutableMapping cover `dict[K, V]` (K invariant, V
  covariant).
- **Variant → parametric sealed union assignability.** `Cons[T]` /
  `Cons` (where `type LL[T] = Cons[T] | Nil`) is assignable into
  `LL[T]`. Required for recursive ADT walks like
  `mut cur: LL[T] = self`.
- **`while True:` reachability.** A loop whose body always returns /
  raises on every branch and contains no `break` is recognised as
  exiting; the post-loop point is unreachable and `missing_return`
  doesn't fire.
- **Post-while-loop narrowing.** After `while y is None: y =
  load()` (no `break`), the post-loop `y` is narrowed to non-None.
  Matches pyright / mypy / pyrefly.
- **`assert x is not None` narrows.** The standard Python static-
  checker idiom now works.
- **`*args` / `**kwargs` require annotations** (Rule 1).
  Canonical idiom is `*args: object` / `**kwargs: object`.
- **`extend list:` dispatches on `list[T]`-annotated receivers.**
  The synthetic `__typhon_builtin_ext_list` class shape is consulted
  before attribute_not_found fires.
- **Exhaustive `match` on `T?` recognises built-in class patterns.**
  `case None: ...; case str() as s: ...` against `str?` no longer
  surfaces `missing_return`.
- **`with`-chain explicit `else err: return Err(err)` validates the
  error type** against the function's declared return. Previously
  the check was gated on the synthetic `?`-op temp shape, so a
  `with`-chain could silently return the wrong error class.
- **`func[T](args)` explicit type instantiation** now fires a clear
  check-time error (was: runtime `'function' object is not
  subscriptable`).
- **`comptime let T: type = int`** lowers to a PEP 695 `type T = int`
  alias statement so `T` is substitutable wherever a type is
  expected. tyc-db also runs the substitution before parsing the
  resolved module so check mode sees the same shape as build.
- **`freeze let X = <expr>` validates the freezability of the RHS at
  check time.** New `tyc::freeze_not_freezable` fires when the RHS
  constructs a non-`frozen` user class, instead of letting the
  failure surface as a runtime TypeError at first import.
- **`pub *` name collisions surface in `tyc check`** (B28). The
  detection logic from `tyc build` is exposed as
  `detect_pub_star_diagnostics` and called from the check command
  before the per-file loop so CI catches collisions before they
  reach build.

### Added — diagnostics polish

- **`interface_not_conforming` arity message** now reads "got N
  non-self parameter(s), expected M" instead of the ambiguous
  "arity N; expected M".
- **`invalid_question_op` help text** mentions both the
  Result-return cause AND the comprehension carve-out.
- **Sealed-union impl distribution dedupe.** `impl Alias:` over a
  sealed union duplicates each method body across every variant; the
  type-checker dedupes diagnostics by `(code, rendered message)` so
  a 10-variant union no longer reports 10 identical errors.
- **`class_attr_shadows_slot` no longer false-positives** on a class
  whose only annotated defaults are mutable literals (`list[str] =
  []` etc.). Those become `default_factory` per-instance fields, not
  shared constants.
- **`MissingAnnotation` text** drops double-backtick wrapping
  (was rendered as `` `parameter `x`` ``).

### Added — language docs

- Cheat sheet documents `class X frozen(Base):` (the modifier comes
  BETWEEN the class name and the base list) and the `*args: object`
  / `**kwargs: object` idiom for genuinely variadic functions.

### Known limitations carried forward

- Preprocess line-number leakage (B15): diagnostics still report
  preprocessed-buffer line numbers for `impl Alias:` distribution
  over sealed unions. The dedupe pass above cuts the *count* of
  noise diagnostics, but each surviving diagnostic still points at
  a synthetic line index past EOF of the original source. Tracked
  for the next release — needs a proper source-map rewrite through
  the diagnostic constructors.

## 0.8.1 — 2026-05-26

Point release: fixes a v0.8.0 regression where the widened
`tyc::attribute_not_found` rule false-positived on venv-introspected
third-party Python classes. Strictly a bugfix; no language, runtime,
or stdlib changes beyond the diagnostic carve-out.

### Fixed — type system

- **`tyc::attribute_not_found` no longer fires on venv-introspected
  third-party classes.** The v0.8.0 firing-site widening incorrectly
  trusted shapes built by runtime introspection
  (`inspect.signature(Cls)`) to be method-complete, so
  `obj.method(...)` against any third-party Python class with a known
  `__init__` flagged the call as missing the attribute
  (`uvicorn.Server.serve(...)`, `httpx.AsyncClient.aclose(...)`,
  `fastapi.Request.body(...)`, …). `InterfaceShape` now carries a
  `partial` flag that `class_shape_from_params` sets on every
  venv-derived shape; `class_hierarchy_fully_known` returns `false`
  whenever any class in the inheritance chain is partial, so
  attribute access stays permissive on third-party APIs whose method
  surface we can't see. All fifteen apps under `examples/apps/`
  build clean again.

## 0.8.0 — 2026-05-26

Stress-test sweep: closes 41 findings from a multi-file v0.7.1 stress
report (`stress/v0.7.1-findings.md`) spanning the type checker, VM,
parser, lowering passes, diagnostics, and CLI. The static surface picks
up several long-missing guarantees (attribute access on class instances,
interface parameter conformance, string-literal singleton types), the
tree-walking VM gains arbitrary-precision integers + insertion-ordered
dicts + a much larger native stdlib, and the parser learns five
language-feature scaffolds the docs already advertised.

### Added — type system

- **`tyc::attribute_not_found` now fires on class instances and generic
  classes**, not just `TypeVar`-bounded parameters. Was a documented
  diagnostic with no firing site for the most common case. Foreign
  base classes (any base not in the project shape registry) keep the
  permissive degrade-to-`Unknown` behaviour so adapters around external
  libraries don't get false positives. Skipped in `unsafe:` regions and
  for `__dunder__` / leading-underscore names.
- **Interface parameter type conformance.** `interface_missing_members`
  now compares parameter types position-by-position (contravariant on
  params) in addition to arity, so a `class BadRepo` claiming to
  implement `interface Repo: def save(self, item: str) -> bool` with a
  `def save(self, item: int) -> bool` impl is rejected at conformance
  time.
- **`Type::LitStr(String)` — string-literal singleton types.**
  `type Color = "red" | "green" | "blue"` and `Literal["a", "b"]`
  produce `LitStr` slots in the resulting `Union`, and assignability
  rejects `paint("orange")` against `Color`. Bidirectional inference at
  the call site widens string literals to `LitStr` only when the
  expected type carries one, so unannotated `let s = "hi"` still
  infers plain `str`.
- **`?` propagation inside `with`-chains.** `result_error_mismatch`
  now fires when the implicit return form of `with x = f()?: …`
  routes a mismatching error type through the chain.
- **`tyc::pattern_shadows_outer` fires** when a `match` capture binds
  a name that already exists in the outer scope.
- **`field_default_ordering` skips `ClassVar` fields.** `ClassVar` is
  excluded from the synthesised `__init__`, so the
  "non-default-after-default" rule shouldn't have penalised it.
- **`newtype Foo = "literal"` is rejected** with the new
  `tyc::newtype_invalid_base` diagnostic.
- **Exhaustive-match-with-guards no longer fires `missing_return`**
  when every variant has at least one (possibly-guarded) case. Pragmatic
  heuristic — pathologically-incomplete guard cascades will no longer
  be flagged.
- **Function parameter rebinding requires `mut`**, matching the
  `let`/`mut` rule everywhere else.

### Added — parser & lowering

- **HKT scaffold `class Functor[F[_]]:`** parses (the `[_]` marker
  declares `F` as a 1-arg type constructor; the VM doesn't enforce
  kind structure but the surface compiles).
- **`impl[T] SealedUnionAlias[T]:` distributes the methods** across
  every variant of the sealed-union alias. The synthetic
  `__typhon_impl_*` class name no longer leaks into diagnostics.
- **`class X[T] frozen:`** (generic + frozen) parses cleanly.
- **`async def` in `interface`** bodies auto-completes the `: ...`
  body (sync `def` already did).
- **Outer-annotation tuple unpack** `let (a, b): tuple[int, str] = …`
  is accepted (the per-element form `let (a: int, b: str) = …` was
  the only form previously).

### Added — VM (tree-walking interpreter)

- **Arbitrary-precision integers.** `Value::Int` is now backed by
  `num_bigint::BigInt` everywhere. `2 ** 100` and `fib(99)` no longer
  overflow. **Behaviour change:** programs that relied on the VM's
  silent i64 wrap-around will now produce mathematically-correct
  results.
- **Dict insertion order is preserved.** `RcDict` is now an
  `indexmap::IndexMap`; `del d[k]` / `dict.pop` use `shift_remove`
  to keep ordering stable. **Behaviour change:** the same `.ty` file
  no longer prints dicts in a different order under `tyc run` vs
  `tyc build && python build/main.py`.
- **f-string format flags fully wired.** `0` zero-pad, `#`
  alternate-form (`0x` / `0o` / `0b`), `[fill]align`, sign, width,
  comma, precision, and type all match CPython output.
- **Mapping match patterns** (`case {"type": "circle"}`, `case {…,
  **rest}`) and sequence-with-star patterns (`case [x, *rest, y]`)
  are implemented.
- **Recursion limit raised to 1000** (was 256) to match CPython's
  default.
- **`yield` and `async def`** now emit a clear
  `NotImplementedError` pointing at `tyc build && python` as the
  fallback instead of crashing the interpreter.
- **Extend-builtin rewrites apply in VM mode.** The VM pipeline now
  runs the desugar pass + `rewrite_builtin_extension_calls` before
  interpretation, so `extend str: def slug(...)` works under
  `tyc run`.
- **Subclass constructors inherit fields.** `class Dog(Animal):
  breed: str` accepts `Dog(name=…, age=…, breed=…)`. Implemented in
  desugar so the parent fields are propagated into the subclass body
  before the synthesised `__init__` is generated.
- **`freeze let` and `newtype` shims** (`__typhon_freeze__`,
  `NewType`) are now native builtins so VM mode no longer crashes
  with `NameError` on the lowered call.
- **Larger native stdlib.** Adds `re`, `typing`, `collections`
  (`OrderedDict`, `defaultdict`, `Counter`, `namedtuple`),
  `functools` (`lru_cache`, `cache`, `cached_property`, `reduce`,
  `partial`), `itertools` (`chain`, `count`, `cycle`, `accumulate`,
  `combinations`, `permutations`, `product`, `islice`,
  `takewhile`, `dropwhile`, `groupby`), `dataclasses`, and
  `pathlib`. Caveats documented inline (some `re` flags accepted
  but ignored; `defaultdict` has no auto-default; `count`/`cycle`
  materialise bounded prefixes).
- **`from typing import Callable`** no longer crashes — VM-mode
  lowering strips pure-type imports.

### Added — diagnostics & CLI

- **Synthetic preprocess lines no longer leak into source listings.**
  `SanitisedDiagnostic` wraps every emitted diagnostic and hides the
  `class __typhon_impl_Foo(object):` / `from typhon_runtime import …`
  / `?`-scaffolding lines, restoring the original text for the user.
  Sanitisation is performed once per file, not once per diagnostic.
- **Dedicated parse-error hints** for multi-line `|>` chains (wrap
  in parens) and `freeze let` at non-module scope.
- **`wrong_arg_count` rephrasing** for kw-only mismatches — the
  self-contradictory "expected 2, got 2" message is replaced with
  a "pass them by name" help block.
- **Collection variance hint** suggests `Sequence[Animal]` /
  `Mapping[K, V]` / `frozenset[T]` instead of the previously-unhelpful
  "widen to `list[Animal] | list[Dog]`".
- **Dict-to-model mismatch** points users at the constructor form
  (`UserCreate(name=…, age=…, email=…)`).
- **`tyc check lib.dty`** now accepts a single `.dty` file
  directly and emits a meaningful "no checkable files" message.
- **`tyc run --compile`** rejects single-file inputs up-front with
  an actionable error.
- **`tyc migrate`** strips trivial `__init__` methods and emits the
  resulting class as plain `class` (not `class!`), preserving any
  leading class docstring so CPython still sets `__doc__`.
- **New lint warnings:**
  - `tyc::empty_collection_no_annotation` for `let xs = []`,
    `{}`, `set()` without an annotation or expected type.
  - `tyc::typing_alias_in_annotation` for bare `List[…]`,
    `Optional[…]`, `Dict[…]`, `Union[…]` in annotations
    (consistent with the existing
    `typing_alias_deprecated` import-level diagnostic).
  - `tyc::contains_secret_literal` fires on inline string
    literals named `*_(TOKEN|SECRET|PASSWORD|PWD|KEY|API_KEY)`.

### Changed

- **`unused_import` default severity is now `warn`** (was `error`).
  Most linters treat unused imports as warnings; the previous default
  required a cleanup pass on virtually every test. Set
  `[strictness] unused-import = "error"` in `typhon.toml` to restore
  the old default.
- **`tyc-vm` external dependencies** (`indexmap`, `num-bigint`,
  `num-integer`, `num-traits`, `regex`) are now declared in
  `[workspace.dependencies]` to keep versions aligned across crates.

### Fixed

- **`re.match`** now anchors at the start of the string (was using
  `Regex::find`, which matched anywhere).
- **`bigint_cmp_f64`** compares `BigInt ↔ f64` without precision
  loss for very large operands — previously routed through `f64`
  and could return wrong-direction orderings near the f64 mantissa
  boundary.
- **`import typhon_runtime.freeze` binds the root module** to
  `typhon_runtime` (per Python semantics), restoring access to
  `typhon_runtime.Ok` / `.Err` after dotted imports.
- **`HashKey` equality for `bool ↔ int`** no longer allocates a
  fresh `BigInt` per lookup.
- **`expand_impl_sealed_unions`** correctly emits a blank-line
  separator between duplicated `impl` blocks and stops capturing
  trailing top-level blank lines into the body.

### Known limitations

- **Numeric / bool literal singleton types** (`Literal[1, 2]`,
  `Literal[True]`) still widen to `int` / `bool`. The `Type::LitStr`
  variant covers strings only — numeric singletons are tracked
  separately for a future round.
- **VM `lazy let` inside a class** uses an identity decorator;
  callers must use the method-call form `obj.x()`.
- **Generators (`yield`) and `async def`** are still not executable
  in the VM; the error message now points at `tyc build` as the
  fallback.

## 0.7.1 — 2026-05-26

Point release: fixes a long-standing semantic-tokens misalignment in
the VS Code language server. Strictly a bugfix; no language, runtime,
or stdlib changes.

### Fixed — LSP

- **Semantic-token positions now line up with the original `.ty`
  source instead of the preprocessed Python view.** The LSP computed
  token coordinates against the post-preprocess source (with `pub`,
  `comptime`, `freeze`, `lazy`, `newtype` line-prefix modifiers
  stripped) but the editor applied those coordinates to the original
  file, so every identifier landed a few characters early — inside
  the stripped keyword instead of on the binding name. Visible
  symptom: `comptime let X` painted `comp` blue + `time let `
  variable-coloured, `pub class Foo` painted `cl` red + `ass Foo`
  class-coloured, `pub newtype TaskId = int` split `newtype` into a
  `TaskId`-coloured prefix and a synthetic-`NewType`-coloured suffix.
  The remap pass now translates token spans into original-source
  coordinates and validates each one by string match, dropping
  synthetic identifiers the preprocessor injects (the `NewType` call
  inside the `newtype` rewrite, the `__typhon_freeze__` wrapper
  around `freeze let` RHS) so the TextMate grammar paints the
  keyword instead of leaking a wrong colour into it. Tokens whose
  preprocessed line index drifted past the original line count
  (from `expand_with_chains`, `expand_gather_blocks`,
  `expand_multiline_guards`) are recovered via a whole-document
  whole-word search, preferring the closest-line occurrence.

## 0.7.0 — 2026-05-25

Carry-over sweep from the Round-3 apps-feedback campaign. Closes the
ergonomics and correctness gaps that survived v0.6.0 / v0.6.1, and
ships the third stress round's findings as compiler features rather
than open issues. Strictly additive: every previously-accepted program
keeps compiling to the same Python.

### Added — language & runtime

- **`pub *` wildcard re-export aggregation in `__init__.ty`,
  including transitive aggregation through sub-packages.** A package
  facade can now write a single `pub *` statement at the top of its
  `__init__.ty` to re-export every direct-sibling module's
  `pub`-marked surface. The build orchestrator collects each
  sibling's top-level `pub` declarations, synthesises a `from
  .sibling import name1, name2; …` block at the `pub *` marker, and
  appends every aggregated name to the synthesised `__all__` —
  matching the surface a hand-written `__init__.ty` would produce
  without the user having to maintain the re-export list as siblings
  evolve.

  Sub-packages are included transitively: when a direct
  sub-directory contains its own `__init__.ty`, the sub-package's
  effective public surface (its own `pub`-marked names, plus
  whatever its own `pub *` aggregates one level deeper) is
  re-exported here. The recursion is cycle-safe via a `visited` set
  keyed on each package directory.

  The marker is preserved on a single line so source maps stay
  byte-aligned.

  Two diagnostics back the feature:

  - **`tyc::pub_name_collision`.** If two siblings both `pub`-export
    the same name, the aggregation would silently shadow one with the
    other in import order. The diagnostic names both sibling modules
    and the colliding name so the user can rename, drop the `pub`, or
    replace `pub *` with an explicit `from .module import …` list.
  - **`tyc::pub_star_outside_init`.** (Advice.) A `pub *` statement
    in a non-`__init__.ty` module is a no-op with confusing intent;
    the diagnostic fires from both `tyc check` and `tyc build` so CI
    surfaces the dead marker.

### Fixed — compiler

- **`tyc-resolve`: declare-only `let NAME: T` (no initialiser) is now
  allowed and integrates with sibling-arm assignment (R3-8 /
  supersedes FINDINGS #91 for the `let` shape).** The
  declare-then-assign-in-arms idiom — `let loaded: Cfg; match _load():
  case Ok(v): loaded = v; case Err(e): return Err(e); …` — previously
  fired `tyc::missing_initialiser`, forcing the user to either
  declare-as-`mut` (losing immutability) or extract a `_load_or_default`
  helper purely to give the binding an initialiser at the declaration
  site. The resolver now tracks each uninitialised `let`-declaration's
  span; the FIRST subsequent assignment to that name silently succeeds
  (it IS the initialiser), and the standard `tyc::immutable_assign`
  fires on any SECOND assignment.
  Sibling `match` arms and sibling `if` / `elif` / `else` bodies each
  count as a separate first-assignment path — the resolver snapshots
  the uninit-span set per arm and unions the initialisations
  afterwards, so the natural `case Ok(v): loaded = v / case Err(_):
  loaded = default()` and `if cond: x = a / else: x = b` shapes check
  clean while retaining `let` immutability outside the branching
  region.
  `mut NAME: T` without an initialiser is also accepted, and follows
  the usual `mut` semantics (any number of subsequent assignments are
  legal).
- **`tyc-types`: definite-assignment analysis for `let NAME: T`
  declare-only bindings (`tyc::use_of_uninitialised`).** Companion to
  the resolver relaxation above. The DA pass walks each function body
  once, tracking the "definitely-assigned" set at every control-flow
  point: `if` / `match` branches intersect (only assignments that
  happen on EVERY non-diverging arm propagate out), `return` /
  `raise` / `continue` / `break` mark a branch as diverging (excluded
  from the intersection), and loops do not propagate assignments out
  (the body may execute zero times). `match` over a sealed union or
  `Result[T, E]` is treated as exhaustive when every variant is
  covered by a class pattern; the canonical
  `case Ok(v): loaded = v / case Err(e): return Err(e)` shape works
  without a `case _:` wildcard. Reads on a path where the binding
  hasn't been assigned fire `tyc::use_of_uninitialised` with labels
  on both the use site and the declaration.
- **`tyc-types`: `with cm() as r:` yield-local resolution (R3-3
  follow-up).** The earlier landing only resolved literal /
  `Class(...)` / `None` yield payloads. The pre-scan now also
  collects every annotated-local declaration in the contextmanager
  factory's body (`let s: Session = …`) and consults that map when
  the yield is a bare name. The canonical "open into a local then
  yield" shape (`let s: Session = …; yield s`) now types `r` as
  `Session` instead of falling through to `Unknown`. Bareword-bound
  locals without annotations still leave `r` as Unknown (a full fix
  would need to thread the inner env through the consumer site;
  authors can add an explicit annotation as the durable workaround).
- **`tyc-types`: `with cm() as r:` / `async with cm() as r:` now types
  `r` from `__enter__` / `__aenter__` methods and from
  `@contextmanager` / `@asynccontextmanager`-decorated generator
  factories (R3-3).** Previously `r` was bound to `Type::Unknown` in
  every shape, so `r.no_such_attr` and `r` flowing into a typed slot
  both slipped past the checker (Unknown is assignable to anything).
  Three lookup paths in priority order:
  1. **Decorator-aware yield-type inference** — a function decorated
     with `@contextmanager` or `@asynccontextmanager` is pre-scanned
     for its first `yield` expression; calling that function in a
     `with` / `async with` head binds the as-target to the yield
     type. Covers the canonical
     `@asynccontextmanager async def session() -> AsyncIterator[Session]:
     yield Session(...)` factory shape that all five Round-3 apps
     wanted to use but had to type-erase to keep checking.
  2. **Concrete-class `__enter__` / `__aenter__`** — a user-defined
     `class Lock:` with `def __enter__(self) -> Lock: return self`
     now propagates the method's return type to `r`.
  3. **Fall-through to `Unknown`** when neither path resolves, so the
     stdlib `with open(p) as f:` shape (whose `__enter__` lives behind
     a stub layer that doesn't carry annotations today) keeps its
     permissive behaviour. No regressions in the apps.

  The decorator pre-scan only resolves literal / `Class(...)`
  constructor / `None` yield payloads — yielding a local binding
  (`s = Session(); yield s`) still leaves `r` as Unknown because the
  pre-scan runs before the body's local-typing pass. Authors can work
  around with a re-annotation inside the body
  (`let typed: Session = r`); a full fix would re-infer the yield at
  the consumer site with the factory's local env.
- **`tyc-types`: `await` on a `Callable[..., Awaitable[T]]` /
  `Callable[..., Coroutine[Y, S, T]]` call now unwraps to `T`
  (R3-1).** The canonical async-middleware shape across
  FastAPI / Starlette / aiohttp is `next: Callable[[Req],
  Awaitable[Resp]]`; calling `next(req)` infers to `Awaitable[Resp]`
  from the `Callable` return position, and `await next(req)` should
  consume the awaitable and produce `Resp`. Without the unwrap, the
  natural shape `let r: Resp = await next(req)` failed with a spurious
  `tyc::type_mismatch: expected Resp, found Awaitable[Resp]` even
  though the runtime behaviour is correct — and the 14-api-gateway
  app had to abandon the recursive `next`-style middleware chain in
  favour of a sync `pre_hook` / `post_hook` pipeline, losing the
  ability to wrap async sections around inner handlers. The single
  biggest Round-3 finding. The canonical `async def f() -> T:` path
  is unaffected: the checker already tracks async functions as
  returning `T` directly (not `Awaitable[T]`), so the new
  unwrap-on-await arms only fire when the wrapper actually appears.
- **`tyc-types`: same-newtype arithmetic preserves the newtype, and
  one-sided literal arithmetic likewise (R2-12).** `newtype LogIndex
  = int` previously inferred `last_idx + 1` and `LogIndex + LogIndex`
  to `Type::Unknown` because the conservative numeric arm of `BinOp`
  inference only matched `(Int, Int)` / `(Float, Float)` — every
  newtype operand fell through and the result silently widened away
  the nominal tag. Raft commit-index advance, ELO updates, watermark
  math, byte offsets, and log indices all paid the same 5–10% LoC tax
  in `int(...) → Wrap(...)` round-trips just to get the types back.
  The new carve-out preserves `LogIndex` across `+ - * // % **` for
  `LogIndex + LogIndex` and `LogIndex + <literal of base>`; `/` keeps
  the existing widening to `float` because Python's `/` is always true
  division. Two **distinct** newtypes with the same numeric base
  (`LogIndex + Term`) fire the existing `tyc::operator_type_mismatch`
  diagnostic — the whole point of `newtype` is that cross-axis math
  must be opted in to. Six new `newtype_arith_*` regression tests
  guard the rule.

### Fixed — checker (also from main's Round-3 sweep)

- **R3-9: ternary `body if test else orelse` now narrows just like
  `if`/`else`.** `isinstance(x, T)` and `x is not None` refine `x`
  on the truthy side (and the negated form on the falsy side) inside
  the expression form, matching the statement-level behaviour.
- **R3-11: class field defaults must order non-default fields before
  defaulted ones.** The synthesised `__init__` follows declaration
  order, and Python rejects a non-default parameter after a default
  one — left unchecked, the class definition blew up at *import*
  time with a misleading `TypeError`. New diagnostic
  `tyc::field_default_ordering` catches this at check time.
- **R3-15: cross-module generic method dispatch propagates class
  TypeVars.** `s: Stream[int].map(f)` now records
  `Callable[[int], U]` as the expected parameter and returns
  `Stream[U]` bound at the call site via the existing PEP 695
  inference. Same fix benefits field access — `let r:
  RecordEnv[int]; r.payload` is now `int`, not the bare `T`.

### Fixed — resolver (from main's Round-3 sweep)

- **R3-4: `from X import Y` inside `if`/`for`/`while`/`with`/`try`/
  `match` arms now binds.** The parser already accepted it; the
  resolver silently skipped nested imports.
- **R3-5: sibling `if` / `elif` branches no longer trip
  `no_block_shadow` for same-named `let` bindings.** The
  per-branch drain/restore that already worked for `case` arms now
  applies to `if` clauses too.

### Fixed — syntax (from main's Round-3 sweep)

- **R3-2: multi-line `go expr(...)` calls now parse.** Implicit line
  continuation inside parens works everywhere else in Typhon.

### Docs (from main's Round-3 sweep)

- **R3-7: argparse-with-typed-dataclass-adapter pattern documented.**
- **R3-10: parametric sealed unions documented.**
- **R3-12: nested `def` and nested `from X import Y` sanctioned.**
- **R3-13: nullary sealed-union variants documented as
  `class Foo frozen: pass` with `case Foo():` matching.**

### Fixed — formatter

- **`tyc fmt`: scientific-notation literals (`1e-12`, `2.5E+7`,
  `1.0e-12`) stop being split into a syntax error.** The in-process
  whitespace pass already inserted PEP 8 spaces around binary `+` /
  `-` when the previous and next characters both looked like
  expression operands; the `e` in `1e-12` is alphanumeric, so the
  heuristic fired on every scientific literal and produced
  `u = 1e - 12` (Python rejects with `SyntaxError: invalid decimal
  literal`). Adds a carve-out that recognises an exponent sign by
  walking back over the trailing digit-run (`<digit>+e` or
  `<digit>+.<digit>+e`, including PEP 515 `_` separators) and
  verifying token-boundary preconditions before classifying the
  trailing `e`/`E` as an exponent marker: the mantissa run must
  contain at least one digit, must not start with `_` (which would
  make it an identifier like `_1e-12`), and must not be preceded by
  an identifier-continuation byte (alphanumeric / `_` / non-ASCII —
  so `value1e-3` correctly keeps the binary-minus spacing). Five
  regression cases in `tyc-format` cover `1e-12`, `2.5E+7`,
  `1.0e-12`, `1_000e-3`, the bare-identifier case (`e - 1`), the
  `<ident>e-N` case (`abc1e - 12`), and the `_<digit>e-N` case
  (`_1e - 12`). Surfaced by the v0.7.0 reorganisation of
  `examples/apps/13-vector-db/` — the `1.0e-12` clamp in HNSW's
  `_random_level` would otherwise emit as `1e - 12` and crash at
  import time.

### Fixed — checker

- **`tyc::stdlib_module_shadow` no longer fires for files nested in a
  sub-package.** A `src/indexer/tokenize.ty` lowers to
  `build/indexer/tokenize.py`, which is *not* on `sys.path` when
  `python build/main.py` runs — the file cannot intercept stdlib
  `import tokenize` from anywhere. The warning now only fires when
  the file sits AT the top of the configured source directory, gated
  via a canonicalised-path comparison: the caller pre-canonicalises
  `project_root.join(config.project.src)` once, and the per-file
  check canonicalises `path.parent()` and compares for equality. The
  canonical-path form correctly handles both edge cases the original
  basename comparison missed: `[project] src = "."` projects fire
  the warning for top-level `.ty` files (which the basename form
  silently suppressed because `parent.file_name()` resolves to the
  project directory name, never `"."`); and a nested
  `src/indexer/src/tokenize.ty` does NOT false-positive against
  the configured src root just because its parent dir happens to
  be named `src`. Three new regression tests guard the nested-file
  case, the `src = "."` case, and the false-positive same-named-
  nested-dir case.

### Added — examples & tooling

- **Every `examples/apps/` project re-organised into grouped
  subdirectories.** The flat-`src/` layout used by all fifteen apps
  through v0.6.x is replaced with 2–6 grouped subdirectories per app
  (`domain/`, `runtime/`, `storage/`, `transport/`, `middleware/`,
  `consensus/`, …) keyed on responsibility, with each package opting
  into `pub *` re-export aggregation via a one-line `__init__.ty`.
  Cross-package imports stay short (`from domain import EntityId`),
  intra-package imports use the relative form (`from .ids import
  EntityId`), and the underlying file count per app is unchanged.
  Every reorganised app still `tyc check`s clean, `tyc build`s, and
  exercises end-to-end through CPython 3.13.
- **VS Code extension `0.1.9 → 0.2.0`.** Grammar catches up to the
  v0.7.0 language surface:
  - `pub def` / `pub async def` now highlight `pub` as
    `storage.modifier.pub.typhon` and the function name as
    `entity.name.function.typhon`; previously `pub` fell through to a
    generic keyword fallback and the `def` rule started one token
    later, producing a different colour for `pub def foo` vs `def
    foo` in some themes.
  - `pub *` at module level (in `__init__.ty`) is now recognised as a
    single re-export construct — the `pub` paints as the visibility
    modifier and the `*` as `keyword.operator.wildcard.typhon`, so
    the marker pops visually instead of falling through to two
    independent keywords.
  - `editors/vscode/README.md` adds `pub` to the modifier list and
    documents the `pub *` re-export and `newtype` constructs.

## 0.6.1 — 2026-05-25

Polish release on top of v0.6.0. Tightens the VS Code TextMate grammar
around `let`-binding annotations and reorganises the build output
directory so emitted `.py` artefacts no longer interleave with their
`.py.map` sidecars.

No previously-accepted program changes behaviour. Existing build dirs
written by v0.6.0 keep working — every map-consumer (`tyc trace`,
`tyc debug`, `tyc ty`) falls back to the legacy adjacent layout when
no `.sourcemaps/` subtree is present.

### Fixed — vscode (`editors/vscode` 0.1.8 → 0.1.9)

- **`let name: lowercase_type = …` now highlights as a type annotation.**
  The standalone `binding-declaration` rule was a one-shot `match` that
  stopped at the binding name, leaving the `:` and the type to fall
  through to the shared `type-annotation` rule. That rule rejects bare
  lowercase identifiers on purpose (to avoid mis-scoping `if cond:`
  and `case Foo(x):`), so `let now: datetime = datetime.now()` painted
  the colon as a generic separator and `datetime` as `variable.other`
  — visibly inconsistent with `let cfg: SchedulerConfig` (uppercase →
  correct), `let n: int` (builtin → correct), and `let c:
  sqlite3.Connection` (dotted → correct). `binding-declaration` is now
  a `begin`/`end` rule that owns the `: Type` slot, routing the
  right-hand side through `type-expression`. A new Unicode-aware
  lowercase fallback in `type-expression` paints `datetime`, `asyncio`,
  and user-defined aliases as `entity.name.type.typhon`.

### Changed — build output layout

- **`.py.map` sidecars now live under `<out>/.sourcemaps/`.** Mirrors
  the emitted Python tree (`build/foo.py` → `build/.sourcemaps/foo.py.map`,
  `build/pkg/bar.py` → `build/.sourcemaps/pkg/bar.py.map`), so the
  build directory is no longer cluttered by interleaved map files.
  Map resolvers in `tyc trace`, `tyc debug`, and `tyc ty` discover the
  new location by walking up from the `.py` and explicitly prefer
  `.sourcemaps/` over the legacy adjacent layout so a stale sidecar
  left over from an upgrade can't shadow a fresh map.

## 0.6.0 — 2026-05-25

Apps-feedback minor release. A two-round stress campaign that built
ten multi-file production-shaped apps on top of v0.5.2 (event-sourced
banking, distributed key-value store, mini-compiler, search engine,
GraphQL server, game ECS, trading engine, ML orchestrator, web
crawler, task scheduler — under `examples/apps/`) surfaced a batch of
correctness and ergonomics gaps. This release closes every issue from
that campaign, adds three additive features (Result method API on
`Ok` / `Err`, `impl` distribution on sealed-union aliases, and a new
`tyc::stdlib_module_shadow` warning), and ships the apps themselves
as canonical multi-file reference programs.

No previously-accepted program changes behaviour. The new features
are strictly additive: existing free-function `result.map(...)` calls
keep working alongside the new `ok.map(...)` method form, and the
new warning is non-fatal.

### Added — language & runtime

- **`Ok` / `Err` expose the standard Result combinators as methods
  (R2-6).** `map`, `map_err`, `and_then`, `or_else` are now bound on
  the runtime classes, not just the free functions in
  `typhon_runtime/result.py`. Heterogeneous-error pipelines that
  previously had to materialise a 4-deep `match` tower with `raise
  RuntimeError("unreachable")` trailers can now normalise per stage in
  chain form:

  ```ty
  let toks = tokenize(src).map_err(_lex_to_pipeline)?
  let ast  = parse(toks).map_err(_parse_to_pipeline)?
  let ty   = check(ast).map_err(_type_to_pipeline)?
  ```

  Semantics match the standard algebra: `Ok.map` transforms the value
  while `Ok.map_err` is identity (and vice versa for `Err`);
  `and_then` chains a `Result`-returning op on `Ok`; `or_else`
  recovers from `Err`. A `build_features` integration test verifies
  the methods land in the emitted runtime and the full algebra
  round-trips through Python.
- **`impl` on a sealed-union alias distributes to every variant
  (R2-3).** `impl Event:` where `Event = A | B | …` used to fire
  `tyc::impl_unknown_class` because the impl-merger only looked for a
  single target class. The desugar pass now collects sealed-union
  type aliases and replicates the impl block's methods on every
  variant class, and the type checker mirrors the same fold so method
  lookup resolves without dropping to free functions. Replicated
  bodies retain any `match self:` patterns; per-variant dispatch is
  automatic because the runtime class on `self` only matches its own
  arm. The duplicate-method check from the concrete-class branch is
  mirrored into the union branch, so a method declared on both `impl
  Union:` and `impl Variant:` still fires `tyc::duplicate_method`.
- **`tyc::stdlib_module_shadow` warning (R2-4).** A project `.ty` file
  whose stem matches a Python 3.13 stdlib top-level module (`types`,
  `ast`, `string`, `io`, `json`, `dataclasses`, `logging`, `random`,
  `time`, …) emits `build/<name>.py`, which the default `python
  build/main.py` entry point puts on `sys.path` ahead of the stdlib.
  Transitive imports (e.g. `dataclasses` → `types`) then resolve to
  the project module instead of the standard library, producing
  baffling `ImportError`s blamed on innocent stdlib packages. The new
  warning fires per-file in `tyc check`, points at the rename pattern
  (`lang_types.ty`, `records.ty`, …), and is gated on
  `typhon.toml` — standalone-file checks skip it. Severity is `warn`
  (non-fatal); `tyc explain stdlib_module_shadow` and
  `docs/diagnostics/stdlib_module_shadow.md` are wired up.

### Fixed — compiler

- **`tyc-types`: variant→sealed-union flow now works across module
  boundaries.** A `pub type Event = A | B` declared in `lib.ty` would
  let a variant `A(...)` flow into an `Event`-typed slot within
  `lib.ty`, but a consumer module that imported both the variants and
  the alias hit `tyc::type_mismatch` on the same construction.
  Sealed-union variant tables, like interface declarations, weren't
  included in the cross-module shape extractor. Both `ModuleShapes`
  and `ExternalShapes` now carry `sealed_unions` and `interfaces`
  maps; the CLI / LSP re-key them under each local import name
  (including alias renames) so the consumer's checker sees the same
  union→variant relationship the source module sees.
- **`tyc-types`: cross-module function signatures preserve parameter
  and return types.** Functions imported via `from foo import f` were
  registered with `Type::Unknown` placeholders for every parameter
  and the return type — the `ArityInfo` sidecar carried names +
  counts but no types. A nullable parameter `def takes(p: Price?) ->
  int:` consumed from another module would render in
  `tyc::nullable_use` diagnostics as the literal placeholder `?`
  ("value is `? | None`, where `?` is required"), and silently accept
  any value the caller passed. `ArityInfo` now records `param_types`,
  `kwonly_types`, and `return_type`; the cross-module binding seed
  reads them so an imported function looks identical to a local `def`
  at call sites.
- **`tyc-types`: nullable-use diagnostic no longer renders `?` as the
  expected type (R1#11).** A companion fix to the cross-module
  signature seed: where the bound was still `Type::Unknown`, the
  formatter used to print a bare `?` for the expected type, producing
  the surreal "value is `? | None`, where `?` is required". The
  formatter now substitutes the resolved bound or, if still unknown,
  falls back to a clearer phrasing.
- **`tyc-types`: exhaustive `match` on a sealed union now satisfies
  missing-return analysis for any subject expression.** `match
  get_state(): case A(): ...; case B(): ...` against a sealed union
  returned by a function fired a false-positive `tyc::missing_return`
  because the analyser only inferred match subject types for bare
  names and attribute access. It now falls back to expression
  inference for any subject shape, so function calls, subscript
  expressions, and arbitrary expressions all flow through the
  exhaustiveness path.
- **`tyc-types`: partial keyword pattern satisfies match exhaustiveness
  (R1#9).** `case Foo(field=x):` (which binds only `field` and ignores
  the rest) was treated as non-exhaustive against the variant even
  though Python's `match` accepts it as a structural match on the
  class. Keyword patterns are now folded into the per-variant
  coverage tally the same way positional patterns are.
- **`tyc-resolve`: `let` declarations in sibling `case` arms no longer
  shadow each other.** `case A(): let key = ...; case B(): let key
  = ...` fired `tyc::no_block_shadow` even though at most one arm
  runs at runtime. The resolver now drains arm-local bindings into a
  side buffer between cases (and splices them back after the match
  for `report_unknown_names`), mirroring the per-arm `env.enter()` /
  `env.leave()` the type checker already does.
- **`tyc-resolve`: for-target no longer silently rebinds prior `let`
  bindings (R2-17).** Python's for-target is an assignment, not a
  fresh declaration, so the iteration value overwrites whatever was
  previously bound. The resolver used to trip `tyc::immutable_assign`
  against a prior `let` in the enclosing scope, even when the prior
  let was inside an unrelated sibling for-loop body. The
  sibling-for-loop case was already silenced; this release extends
  the same silencing to any prior binding when the *new* binding is a
  for / with / except / comprehension target. Manual body-level
  assignment (`i = i + 1`) is unaffected — that path goes through
  bareword-assignment resolution, not the loop-target declaration
  path.
- **`tyc-syntax`: `pub freeze let X = …` now parses.** `pub` stacks
  with every other module-level binding modifier per the diagnostic
  docs, but the `pub`-prefix stripper didn't recognise `freeze let`
  as a multi-word keyword form. `pub freeze let DEFAULT: dict[str,
  int] = {...}` now lowers correctly and round-trips through `tyc
  fmt`.
- **`tyc-syntax`: `pub def` is now visible to the `?` operator
  validator (R2-1).** The `?` propagation pass walked function
  declarations to check that the enclosing return type accepted
  `Result`, but the `pub`-prefix stripper ran after the walk so
  `pub def f() -> Result[T, E]:` looked like a bare `def` with no
  return type and the validator gave up. The stripper now runs
  upstream of the validator.
- **`tyc-types`: `loop.run_until_complete(coro())` no longer fires
  `tyc::missing_await`.** The asyncio event-loop method is a
  legitimate consumer of coroutines — same family as `asyncio.run(...)`,
  which the analyser already excluded. Added to the coro-acceptor
  whitelist; the receiver shape is unconstrained because the method
  name itself is the carve-out.
- **`tyc-types`: `@contextmanager` factory bodies are exempt from
  `tyc::resource_not_managed`.** A `@contextmanager`-decorated
  function whose body opens a file or socket *is* the resource
  manager — flagging `let f = open(path)` inside is a false
  positive. The check now skips function bodies carrying
  `@contextmanager` or `@asynccontextmanager` (bare or dotted-module
  form).

### Improved — diagnostics

- **`tyc::type_mismatch` help text now suggests widening the
  annotation, not narrowing it to the found type.** The previous help
  text — "change the value, or update the annotation to `<found>`" —
  implicitly suggested dropping the expected type, which is almost
  never what the user wants. Now reads "change the value so it
  produces `<expected>`, or widen the annotation to `<expected> |
  <found>` if both are intended", which matches typed Python's
  culture and the way users actually fix the error.

### Improved — docs

- **`docs/diagnostics/stdlib_module_shadow.md`** ships as a new
  catalog page covering the rationale, the `ImportError` cascade it
  prevents, and a rename table for the most common collisions
  (`types.ty → lang_types.ty`, `dataclasses.ty → records.ty`, etc.).
- **`docs/diagnostics/class_attr_shadows_slot.md`** gains a new
  "Alternative: nullary sealed-union variants" section pointing at
  the `pub class TyInt frozen: pass` idiom instead of the
  `placeholder: int = 0` workaround.
- **`docs/guides/07-sealed-unions-and-match.md`** now documents
  keyword patterns (`case TaskStarted(task_id=tid):`) as the
  recommended form for variants with more than two or three fields —
  positional patterns must match field count exactly, keyword
  patterns don't, and Python's `match` supports the latter natively.
- **`docs/cli.md`** documents `tyc explain --list` (which the
  diagnostic hint footers have always advertised but the command
  table never mentioned).

### Added — examples

- **`examples/apps/` — ten production-shaped multi-file apps** built
  on top of v0.5.2 as a stress harness: task scheduler, trading
  engine, ML orchestrator, event-sourced banking, web crawler,
  GraphQL server, game ECS, mini-compiler, search engine, distributed
  KV. Each ships under a `typhon.toml`, `tyc check`s clean, `tyc
  build`s, and runs through CPython. Every app carries a
  `FRICTION.md` or per-app README noting which compiler / ergonomics
  gaps the build surfaced — those gaps are the source list for the
  fixes and features above. `examples/apps/TYPHON_FEEDBACK.md`
  aggregates the campaign findings.

### Changed — VS Code extension

- **Version `0.1.7 → 0.1.8`.** No new keywords or grammar surface in
  this batch (the deep grammar audit in v0.5.1 already covers
  `pub freeze let`, sealed-union impls, the `?` operator, and the
  `lazy_let` runtime helper). The bump tracks the new tyc release.

### Fixed — LSP semantic tokens

- **`newtype` declarations now paint as `class` everywhere.** `newtype
  DocId = int` desugars to `DocId = NewType("DocId", int)`, which the
  resolver registers as a `BindingKind::Value` — same kind a plain
  `let x = 1` gets. Every reference site (`def f(id: DocId) -> DocId:`)
  was therefore emitted as the generic `variable` semantic-token type
  and rendered as the local-variable colour (light blue in Dark+,
  plain text in most user themes) while real classes like `Document`
  defined right next to them rendered as `class`. The user's
  screenshot of `examples/apps/09-search-engine/src/index.ty`
  surfaced exactly this asymmetry. `tyc-lsp::semantic` now walks the
  module for `NAME = NewType("NAME", BASE)` shapes and promotes both
  the declaration and every reference to the `class` token, matching
  how `pub class X:` declarations already paint.
- **Class-body field declarations now paint as `property` instead of
  `variable`.** `pub class Document: id: int` used to emit `id` as a
  local-variable token even though it's a dataclass slot, which made
  field names render with the local-binding colour rather than the
  property colour Pylance gives Python `@dataclass` fields. The token
  type now derives from the binding's enclosing scope kind so
  class-body Value bindings emit as `property` and function/module
  Value bindings stay as `variable`. Keeps the in-class declaration
  consistent with the `obj.field` access elsewhere in the file.

## 0.5.2

A correctness + documentation point release on top of v0.5.1. Two
compiler bugs surfaced during a comprehensive audit of the
`docs-site/` example corpus are fixed; the docs site itself gets a
~40-file sweep adding side-by-side Typhon / Emitted-Python tabs to
every complete-program example; and a re-runnable audit harness lands
under `docs-site/scripts/` so the same sweep can run in CI.

No language semantics change beyond accepting two previously-rejected
forms (`set - set` and `<ident>?` propagation), and no
previously-accepted program changes behaviour.

### Fixed — compiler

- **`tyc-syntax`: `<ident>?` in value position now lowers to the standard `__typhon_q_N__` ladder.**
  `let x: T = a?` after a `gather:` block (or any time the operand
  was a bare identifier rather than a paren-prefixed call `f()?`)
  used to be silently rewritten as `x: T = a | None`, then crash at
  runtime with `TypeError: unsupported operand type(s) for |: 'Ok'
  and 'NoneType'`. The propagation pass only recognised `f()?`
  because it required the character before `?` to be `)`. It now
  disambiguates by RHS position: when the line has an `=` and the
  identifier-then-`?` sits on the RHS, treat the trailing `?` as
  propagation and emit the standard `__typhon_q_N__` ladder. Pure
  annotation forms (`let x: int?`, `let x: list[int]?`) and type
  aliases (`type X = T?`, `newtype Y = T?`) are unchanged. Unblocks
  the natural `gather:` → `?` → `Ok(...)` shape used across the
  tour, first-program, and recipes docs.
- **`tyc-types`: `set - set` and `frozenset - frozenset` now type-check as Python set difference.**
  The operator-compatibility check for `-` accepted only numeric
  operands; the Python set-difference form (`{1, 2, 3} - {2, 3, 4}`)
  fired `tyc::operator_type_mismatch` even though the sibling bitwise
  operators `&`, `|`, `^` (which fall through to the permissive arm)
  worked. Added an explicit carve-out for `Operator::Sub` between two
  `set` / `frozenset` operands, matching the existing carve-outs for
  `+` on `list` / `list` and `tuple` / `tuple`. Closes the
  `types/collections.mdx` set-operations example.

### Changed — documentation

A comprehensive audit of the `docs-site/` example corpus rolled out
across roughly 40 `.mdx` files. Every complete, runnable Typhon
example in the tour, types, reference, recipes, getting-started, and
lowering sections is now presented as a side-by-side
`<Tabs><TabItem label="Typhon"/><TabItem label="Emitted Python"/></Tabs>`
pair, with the Python side produced by running the example through
the actual `tyc build` pipeline rather than written by hand. Partial
illustrative snippets and intentional negative examples are left as
plain `python` fences.

Stale or incorrect doc claims that the audit surfaced were also
fixed:

- **`lazy let X: T:` colon-block form (`reference/lazy.mdx`,
  `lowering/lazy.mdx`)** was shown for class-level `lazy let` but
  doesn't parse. The correct form is `lazy let X: T = expr`. The
  generator-style `lazy[T]` return-type form is documented as
  designed-but-not-yet-implemented, since the parser doesn't
  recognise `lazy` in return-type position today.
- **Multi-line `|>` pipes without wrapping parens
  (`reference/pipes.mdx`)** fail parsing the same way any multi-line
  Python expression does. Updated to use the working
  `let slug: str = (...)` parens-wrapped shape and noted the
  requirement.
- **`model X frozen:` (`interop/pydantic.mdx`)** isn't parsed — the
  `frozen` modifier is recognised on `class` only. Replaced with a
  `.py` escape-hatch example and a roadmap note.
- **`let`-shadowing (`reference/let-mut.mdx`,
  `diagnostics/binding-errors.mdx`, `tour/five-rules.mdx`)** was
  documented in two places as a supported fix for `tyc::no_block_shadow`,
  but the checker rejects every form of `let`-shadowing because
  Python is function-scoped and a nested `let x` would silently
  rebind the outer binding. Corrected to recommend `mut` or a fresh
  name.
- **`tour/control-flow.mdx`** said static fall-through analysis was
  "reserved for a future release". It isn't — the checker enforces
  `tyc::missing_return` today. Converted the example to a clearly-
  marked negative case.
- **`lowering/runtime.mdx`** was rewritten to show the exact
  emitted-runtime source as of this release: `__init__.py`,
  `tasks.py`, `lazy.py`, `freeze.py`, plus the `parallel.py` /
  `stdlib.py` / `result.py` siblings. The earlier excerpts were
  simplified and out of date.
- **`diagnostics/compile-errors.mdx`**: the lazy-let example was
  written inside the `class` body (which fires
  `tyc::method_in_class_body`); moved into an `impl` block where the
  feature actually belongs.

### Added — tooling

`docs-site/scripts/verify_examples.py` walks every `.mdx` under
`src/content/docs/`, extracts python / typhon / ty code blocks,
classifies each one (partial snippet / intentional negative /
complete program / emitted-Python block), and tries to compile every
complete-program block via `tyc build` in a temporary project.
`--real-only` filters out the partial-snippet noise that fires when a
small example references a class defined earlier on the same page.
Exits non-zero when real issues remain so this can wire into CI.

## 0.5.1

A correctness + tooling point release on top of v0.5.0. Two compiler
bugs that PR #120 reviewers caught on the new examples corpus are
fixed; the VS Code TextMate grammar gets a deep audit (PRs #119, #121)
that closes ~18 miscolorings across real Typhon code; and the
examples directory grows by 22 stdlib-only exercises (47–68) plus
emitted `.py` companions for every example shipped to date.

No language semantics change — every previously-accepted program
still compiles, and the three compiler fixes only narrow the set of
*emitted-Python* bugs the toolchain produces.

### Fixed — compiler

- **`tyc-format`: triple-quote tracker desync (PR #120).** The
  in-process whitespace pass treated `"""` as three independent
  toggles of its single-quote tracker. On a line like
  `""", encoding="utf-8")` (which `ruff format` produces when it
  reflows `"\n".join(xs) + "\n"` into triple-quoted form), the
  tracker came out of sync and rewrote `"utf-8"` as `"utf - 8"` —
  an encoding name Python rejects with `LookupError` at runtime.
  Added a triple-quote state machine that consumes everything up to
  the matching `qqq` closer verbatim, plus a regression test. Two
  PR reviewers flagged this on examples 16 and 21.
- **`tyc-desugar`: pattern-walk skipped `case Ok(...)` / `case Err(...)`
  (PR #120).** `stmts_use_result_names` walked match-case bodies and
  guards but skipped the patterns themselves, so a file that only
  ever pattern-matched on `Ok` / `Err` (without returning or
  constructing a `Result` anywhere) didn't trigger the
  `from typhon_runtime import Ok, Err, Result` auto-injection. The
  emitted `.py` then `NameError`'d at runtime — caught on
  `examples/47-mini-app/src/api.py` and previously worked around in
  `examples/testing/test_calculator.ty`. Added
  `pattern_uses_result_names` that walks every `Pattern` variant
  (`MatchValue`, `MatchSequence`, `MatchMapping`, `MatchClass`,
  `MatchAs`, `MatchOr`, `MatchStar`, `MatchSingleton`).
- **`tyc-syntax`: duplicate `__typhon_Err__` alias imports (PR #120).**
  `prepend_typhon_err_alias_import` runs once per pipeline pass that
  introduces `?` propagation or `with`-chain lowering, with a
  header-scan check meant to suppress duplicates when an earlier pass
  already injected the alias. The check compared `trimmed` (still
  carrying its trailing newline) against `IMPORT_LINE.trim_end()` (no
  newline), so equality never matched and pass 2 injected a second
  copy directly above pass 1's. Strip the trailing newline before
  comparing; added a regression test; rebuilt
  `examples/07-error-handling/error_handling.py` to confirm only one
  alias line ships.

### Changed — VS Code extension (PRs #119, #121)

The TextMate grammar got a deep audit against ~19k lines of real
Typhon code from `examples/` and `stress/` plus a hand-built corpus
exercising every language construct. Tokenisation was driven through
`vscode-textmate` + `vscode-oniguruma` and every flagged span fixed.
The extension version bumps `0.1.5 → 0.1.7`.

- **Split `expression` into `expression` + `expression-inner`.**
  Subscripts `[...]`, dict / set literals `{...}`, and lambda bodies
  now route through `expression-inner` and handle their own `:` so it
  stops being eaten as a spurious type annotation. Fixes
  `xs[1:4:2]`, `{k: v for k, v in xs}` (the comprehension `for` was
  being scoped as a type-expression identifier), and
  `lambda x: x + 1` (the body was parsed as a type).
- **Balanced `#parens` matcher in expression-inner** so nested parens
  in parameter defaults (`s: T = Depends(get_store)`) consume their
  own `)` instead of letting it close the outer parameter list.
- **Tightened type-annotation lookahead.** The colon in single-line
  statements like `if isinstance(...): return left + right` and
  `case Foo(x): bar()` no longer fires as a type annotation. The
  check now requires an uppercase identifier, a builtin type name, a
  dotted lowercase name (`pd.DataFrame`), or a type-shaping
  punctuation (`[`, `(`, quote, …) to follow.
- **~18 additional miscoloring fixes** across keyword spans
  (`pub` / `freeze` / `extend` / `unsafe`), f-string nesting, regex
  literals, `match` arms, and decorator argument lists. See PR #121
  for the full list.

### Added — examples (PRs #119, #120)

- **22 new stdlib-only exercises (examples 47–68).** Practical Typhon
  programs that emit valid Python without any third-party
  dependencies: a mini-app (47), `newtype` IDs (48), Fibonacci memo
  (49), linked list (50), BST (51), stack & queue (52), sorting (53),
  graph traversal (54), word frequency (55), state machine (56),
  iterators / generators (57), context managers (58), matrix ops
  (59), Caesar cipher (60), tic-tac-toe (61), priority queue (62),
  event bus (63), URL router (64), INI parser (65), rate limiter
  (66), trie (67), JSON-RPC builder (68).
- **Emitted `.py` companions** for every example 01–46 so readers can
  see the lowering without running `tyc build`.
- **`examples/testing/`** gains a calculator + pytest companion that
  exercises the `Result`-in-tests pattern.

## 0.5.0

The post-v0.4 roadmap-sweep release. v0.5.0 lands the seven Phase
4+ items shipped on `claude/typhon-roadmap-items-nCs4K` (PR #105)
plus four follow-up epics that close the open frontier work flagged
during that sweep (PRs #110, #111, #112, #113):

1. The Salsa boundary now caches the parse + resolve pair behind a
   single `preprocessed_full` query and `check_source_file_with_imports`
   reuses the cached diagnostics instead of double-resolving (PR #112).
2. The auto-parallel comprehension rewriter grew dict-comp support
   alongside the existing list-comp + set-comp coverage, and the
   `tyc debug` wrapper now reads in full Typhon coordinates (`where`,
   `list`, prompt, frame summary) instead of just the post-pause
   banner. `tyc ty`'s diagnostic remapper handles paths with spaces
   by walking left until a candidate path resolves to a real
   `.py.map` (PR #111).
3. `Type::TypeConstructor` and `ComptimeValue::Type` give the type
   system a foothold for higher-kinded types and types-as-comptime-
   values without committing to the full HKT surface — the parser
   accepts `F[_]` parameter syntax in class generics, and
   `comptime let T: type = int` round-trips through emit (PR #113).
4. Corpus coverage gains the PyPI sweep harness (3+ third-party
   packages round-tripped through `tyc migrate` + `tyc build` with
   semantic-diff against `python -m foo.bar`), a fourth
   `python_semantic_drift` audit round (17 fresh probes, all green),
   and a design doc for the general inter-procedural field-init
   audit that PR #105's trivial-factory work intentionally scoped
   out (PR #110).

This is a minor release because the type-system additions in (3)
expand the accepted surface — programs that previously failed parse
or type-check on `F[_]` / `comptime let T: type = ...` will now
compile. No previously-rejected runtime semantic is newly accepted.

### Added — incremental compilation

- **Salsa cache shared across `check_file_with_imports` (PR #105).**
  New `preprocessed_full` tracked query returns the full
  `PreprocessResult` so `preprocessed_text`, `resolved_module`,
  `module_decl_names`, and the new `check_source_file_with_imports`
  entry point all share one preprocess pass per revision. The LSP
  now calls the SourceFile-backed entry directly so a per-keystroke
  cross-module check hits the cached parse + resolve on every
  unchanged sibling; only the type-check (which depends on the
  per-invocation cross-module shape registry) actually re-runs.
- **Eliminated double-resolve in `check_source_file_with_imports`
  (PR #112).** `ArcResolvedModule` now carries the resolver
  diagnostics alongside the resolved module (both behind `Arc` for
  pointer-equality `salsa::Update`), so the second
  `resolve_module_with` call that previously ran just to harvest
  diagnostics is gone. Every consumer of `resolved_module` (LSP
  hover, definition, the multi-import check path) shares the same
  cached output.

### Added — `tyc ty` integration

- **Diagnostic attribution for `ty` output (PR #105).** `tyc ty` now
  captures the `ty` subprocess output and rewrites every
  `path.py:LINE[:COL]:` reference to the originating Typhon source
  via the adjacent `.py.map` sidecars. Pass `--raw` to forward
  output verbatim. The shared `commands/source_map.rs` module is
  consumed by both `tyc trace` and `tyc ty`.
- **Path-with-spaces handling in the diagnostic remapper (PR #111).**
  `parse_py_ref` now walks left from each `.py:` occurrence and
  yields successively longer candidate prefixes, taking the longest
  match that corresponds to a real `.py.map` sidecar. The lookup is
  cached per-line so the candidate enumeration stays O(1) amortised.

### Added — `tyc debug`

- **Typhon-aware pdb wrapper (PR #105).** `tyc debug` writes a
  one-shot Python wrapper that subclasses `pdb.Pdb` and prints
  `[ty] <src>:<line>` after every pause (entry, breakpoint, step,
  exception). The wrapper loads every `.py.map` sidecar under the
  build directory at startup so per-pause lookup is a dict + list
  dereference. Default-on; pass `--raw-pdb` to launch
  `python -m pdb` directly.
- **Full UI translation: `where`, `list`, prompt, stack summary
  (PR #111).** The pdb subclass now overrides `do_list`,
  `do_where`, `format_stack_entry`, and the `prompt` property so
  the entire debugger surface reads `.ty` coordinates instead of
  the emitted `.py` paths. Source-snippet rendering (`list`) reads
  the `.ty` file slice when a `.py.map` resolves the source path.

### Added — `tyc migrate`

- **Three new line-level rewrites (PR #105).**
  - `@dataclass(frozen=True[, ...])` (and `@dataclasses.dataclass`)
    drops the decorator and the following class header gains a
    trailing `frozen` modifier (`class Vec frozen:`).
  - `class X(Protocol):` and `class X(Protocol[T]):` become
    `interface X:` / `interface X[T]:`. Multi-base forms are left
    untouched.
  - `NAME = NewType("NAME", BASE)` at module level becomes
    `newtype NAME = BASE`. The matching `Protocol` and `NewType`
    `from typing import …` entries are pruned alongside.

### Added — type checker

- **Cross-function field-init audit (PR #105).** A pre-scan
  recognises the trivial factory-helper shape `def make(): return
  X.__new__(X)` (and the two-statement `obj = X.__new__(X); return
  obj` variant). Call sites `let c = make()` register the LHS as a
  tracked uninit instance using the helper's missing field set, so
  a downstream escape fires `tyc::missing_field_init` exactly as if
  the user had constructed the partial instance inline. Helpers
  that do any intervening field assignment are treated as
  initialising the instance properly and are not recorded.
- **Variance table expansion (PR #105).** `generic_param_variance`
  gains `AsyncContextManager`, `KeysView`, `ValuesView`,
  `ItemsView`, `Type` / `type`, and `Counter`.
- **Higher-Kinded Types foundation (PR #113).** New
  `Type::TypeConstructor { name, arity }` variant represents type
  constructors with unbound parameters. `type_from_annotation`
  recognises `F[_]` parameter syntax inside class / function
  generic parameter lists (e.g. `class Functor[F[_]]:`) and
  `walk_typevars` traverses the new variant. The full surface
  (`def map[F[_], A, B](fa: F[A], f: A -> B) -> F[B]`) is staged on
  this scaffold; the design doc `TYPE_SYSTEM_FRONTIER.md` records
  the deferred unification work.

### Added — analyser

- **Parallel comprehensions: set-comp support (PR #105).**
  `{f(x) for x in xs}` now rewrites to
  `set(typhon_runtime.parallel.map_pure(lambda x: f(x), xs))` under
  `[strictness] auto-parallel`. Same eligibility rules as the
  list-comp path; the `set(...)` wrapper preserves the runtime set
  semantics (uniqueness + unordered).
- **Parallel comprehensions: dict-comp support (PR #111).**
  `{k_expr: f(v) for k, v in items}` rewrites to a dict-literal
  unpack form (`{**dict(typhon_runtime.parallel.map_pure(...))}`)
  that avoids the `dict` shadowing concern from the set-comp path.
  Only fires when exactly one of `{key_expr, value_expr}` is a
  pure-call eligible side.
- **`comptime` types-as-values (PR #113).** New
  `ComptimeValue::Type(String)` variant lets `comptime let T: type
  = int` (and any in-scope type name) round-trip through the
  comptime evaluator. The bare-name resolution covers the eight
  primitive heads (`int`, `str`, `bool`, `float`, `bytes`, `None`,
  `type`, `object`); `Any` is intentionally rejected unless
  imported because the emitter cannot synthesise the import.

### Added — test infrastructure

- **Third-party Python corpus round-trip sweep (PR #105).** New
  `stress/third-party-py-corpus/` ships six representative Python
  fixtures (dataclass, frozen dataclass, Protocol, NewType, PEP 695
  generic, legacy `Generic[T]`) and the integration test
  `third_party_corpus_round_trips_cleanly` exercises the full
  `tyc migrate` → `tyc check` chain on each. Failures are collected
  so a single run surfaces the complete regression list.
- **PyPI sweep harness (PR #110).** `stress/pypi-sweep/` ships
  `sweep.py`, a CLI that pip-installs a configured set of typed
  Python packages into a tempdir, runs `tyc migrate` + `tyc build`,
  and compares smoke-script output against the original package.
  The default config covers `attrs`, `click`, and a small
  Pydantic-using package; results land in `findings.md` so
  regressions are tracked across runs. Opt-in nightly job; not
  wired into per-PR CI because the pip install dominates the
  budget.
- **`python_semantic_drift` audit round 4 (PR #110).** Fresh
  17-probe sweep in `stress/round-2026-05-23-drift-round-4/`
  covering walrus-in-comprehension, augmented-assignment narrowing,
  `yield from` generator delegation, match-pattern `*` capture,
  string multiplication, `raise X from Y` typing, and `f(*args,
  **kwargs)` unpacking. All probes accept under `tyc check`; the
  pre-existing closed set carries forward unchanged.
- **Inter-procedural field-init audit design
  (`stress/interprocedural-audit-design.md`, PR #110).** Records
  the summary-IR sketch for generalising PR #105's trivial-factory
  audit to multi-step factories that finish initialisation across a
  call chain. No code change yet — the IR design is the prerequisite.

### Docs

- **`docs/roadmap.md`** updated to mark every Phase 4+ item shipped
  in this release as ✅ complete. The HKT row records the staged
  surface (`Type::TypeConstructor` + `F[_]` param syntax) and
  references the deferred unification work in
  `TYPE_SYSTEM_FRONTIER.md`.
- **`TYPE_SYSTEM_FRONTIER.md`** (new) catalogues the type-system
  work that remains beyond v0.5.0: full HKT unification, variance
  inference on user-declared generics (currently invariant by
  default), and the broader comptime types-as-values story.
- **`stress/EPIC_SUMMARY.md`** (new) summarises the PR #110 epic
  scope — what shipped, what's still open in the inter-procedural
  audit design, and the next-round drift candidates.

## 0.4.0

A correctness sweep on the v0.3.0 surface (PR #103) plus a focused
`tyc::python_semantic_drift` audit catches the highest-impact silent
under-checks left over from the May 2026 round: `bool` is now treated
as a subtype of `int` in assignment / arithmetic / unary contexts,
fixed-arity tuples are covariant on every slot (not just slot 0),
subclass constructors finally see inherited fields, and
`tyc::missing_field_init` no longer loses tracking when a partially-
initialised instance escapes via a container literal or annotated
alias. Emit gains triple-quoted-string preservation so multi-line
docstrings stop round-tripping into single-line `\n`-escaped blobs;
types gains De Morgan narrowing so `not (A or B)` refines both names
in the post-`if` branch. Rounded out by a corpus round-trip CI gate
(every `.ty` under `examples/` must `tyc check` clean on every PR),
re-enabled upstream Ruff insta tests (266 vendored tests back online),
and a guide-10 backfill that finally documents `newtype`, `freeze let`,
and `pub` end-to-end.

This is a minor release because the type-checker behavioural changes
(bool ⊆ int, fixed-arity tuple covariance) are observable from user
code — programs that previously failed will now compile. No
intentionally-rejected program is newly accepted that would change
runtime semantics.

### Added — language

- **`bool ⊆ int` subtype widening.** CPython defines `bool` as a
  strict subclass of `int`, but the v0.3 checker rejected
  `let x: int = True`, `let x: int = 1 + True`, and `let y: int = -True`
  as `tyc::type_mismatch`. New `(Int, Bool)` and `(Float, Bool)` arms
  in `assignable`; the `BinOp` arithmetic case folds `Int | Bool`
  operands into `Int`; unary arithmetic / bitwise on bool yields
  `Int`. One-way only — `let b: bool = 1` still rejects. This was the
  canonical case `tyc::python_semantic_drift` was created to flag.
- **De Morgan narrowing on `not (A or B)`.** `collect_narrowings_inner`
  grows an `Expr::BoolOp` arm: in `if A and B:` both operands narrow
  in the true branch; in `if not (A or B): return` both operands
  narrow in the post-`if` branch (the early-exit refinement). The two
  ambiguous shapes (a bare `or`, `not (and)`) stay conservative since
  no single name can be safely refined. Closes the long-standing gap
  where `if not (x is None or y is None): use(x, y)` still rejected
  the `Optional` unwrap.

### Fixed — type checker

- **Fixed-arity tuple covariance on every slot.**
  `generic_param_variance` only declared `("tuple", 0) => Covariant`,
  so `tuple[int, int]` widened slot 0 to float but rejected slot 1.
  Promoted `tuple` / `Tuple` to a head-level early return so every
  slot is uniformly covariant; soundness preserved because tuples
  are immutable (same reason `tuple[T, ...]` was covariant already).
  Mutable-container invariance for `list` / `dict` / `set` is
  intentionally unchanged — the static rule guards the mutation
  hazard CPython's duck-typing can't see.
- **Subclass constructors accept inherited fields.**
  `class Dog(Animal):` previously rejected `Dog(name="Rex",
  breed="Husky")` with `tyc::unknown_kwarg 'name'` because the arity
  check only saw `Dog`'s direct field surface. Adds `bases:
  Vec<String>` on `InterfaceShape` and an `effective_class_shape()`
  helper that walks the inheritance chain (parent fields first, child
  overrides on collision, cycle guard); used at the constructor call
  site for both the kwarg validator and `class_constructor_arity`.
  Dotted base classes (`class Sub(pkg.Base):`) are recorded too —
  `effective_class_shape` silently skips bases with no shape entry,
  so foreign / unresolved bases are harmless.
- **Dotted-attribute annotations resolve to the foreign class shape.**
  `let c: foo.ApiClient = ...` used to land as `Type::Unknown`,
  silently dropping every downstream method-arity / kwarg check on
  `c`. New `Expr::Attribute` arm in `type_from_annotation` produces
  `Class("{module}.{attr}")` matching the call-site convention; carves
  out the permissive `typing.<X>` surface (`Any`, `Self`, `List`,
  `Dict`, `Tuple`, `Set`, `FrozenSet`, `Sequence`, `Iterable`,
  `Iterator`, `Mapping`, `Callable`, `Optional`, `Union`, `Final`,
  `ClassVar`, `Annotated`, `Literal`, `Type`, `TypeVar`) so a
  `-> typing.Self: return self` fluent-builder doesn't trip
  `type_mismatch`. Multi-segment paths (`a.b.Cls`) still fall back to
  `Unknown` pending full module-registry resolution.
- **`import foo as f; let c: f.Cls` canonicalises against `foo.Cls`.**
  The dotted-annotation arm produced `Class("f.Cls")` while the call
  site `f.Cls(...)` produced `Class("foo.Cls")` because the env
  resolves `f` to `Type::Module("foo")`, causing a spurious
  `type_mismatch` on every aliased-import handoff. Added
  `Checker::canonicalize_module_aliases` (walks `Class` / `Union` /
  `Generic` / `Function` recursively) as a second-chance comparison
  inside `is_assignable`.
- **`tyc::missing_field_init` catches container-literal + alias
  escapes.** A partially-initialised instance flowing into the RHS of
  an annotated assignment used to slip past the audit silently
  because the only tracked escape sites were `return` and call
  arguments. New hook runs `audit_check_escape` on the RHS of every
  `Stmt::AnnAssign` whose value mentions a tracked binding, covering
  `let configs: list[Config] = [c]` (container-literal escape) and
  `let alias: Config = c` (outer-scope alias escape). Self-reference
  in container literals (`let xs: list[T] = [c, c]`) also fires
  correctly. Bare `Stmt::Assign` stays intra-function aliasing and
  is intentionally not audited.
- **`BinOp` no longer over-promotes to float when one operand is a
  foreign class.** The previous `(Float, _) | (_, Float) => Float`
  arm caught `(Float, Class("torch.Tensor"))` and rejected
  `let y: torch.Tensor = 4.0 * x.sum()` despite the runtime
  `__rmul__` returning a Tensor. Now only fires when both sides are
  numeric primitives; otherwise falls through to `Unknown` so the
  surrounding annotation drives. Surfaced organically by the new
  corpus sweep against `examples/33-pytorch-tensors/`.

### Fixed — emit / VM correctness

- **Triple-quoted strings round-trip as triple-quoted.**
  `Expr::StringLiteral` in `tyc-emit` now emits triple quotes when
  the source used triple quotes or when the literal contains a real
  newline, instead of `\n`-escaping multi-line docstrings into
  single-line blobs that every formatter then fights with on save.
  New `escape_triple_quoted_string` helper breaks any internal
  3-quote run so the literal can't close prematurely. Skipped inside
  f-string interpolation (outer is `Some`) since nested triple quotes
  are almost always wrong.

### Internal / chore

- **Corpus round-trip CI sweep (`tests/pipeline.rs`).**
  `corpus_examples_all_check_clean` walks every `.ty` under
  `examples/` (excluding intentional-failure probes in
  `examples/testing/`) and asserts `tyc check` exits 0 on each. Any
  change to the type checker that breaks a previously-working example
  is caught in CI rather than discovered in a manual sweep. Closes
  the long-standing roadmap concrete-next-step #1.
- **Vendored Ruff insta tests re-enabled.** Flipped `[lib] test =
  true` on `ruff_python_parser`, `ruff_python_ast`, `ruff_text_size`;
  brings 266 upstream tests back online (217 + 49 + 0) with zero
  regressions against the Typhon `Mutability` extension. Closes the
  second of the two `tyc/vendor/README.md` deferred follow-ups.
- **`stress/round-2026-05-23-drift/` — three audit rounds.**
  Round 1: 15 probes targeting numeric / container variance under
  `int → float` widening. Round 2: inheritance + dotted-attr
  limitations. Round 3 (`45-probes-round-2-and-3.ty`): protocols,
  decorators, async, generics, exception flow, slicing, star-args,
  optional chaining — 30 probes, all green against the fixed
  checker. Documented per-case in
  `docs/diagnostics/python_semantic_drift.md`.

### Docs

- **Guide 10 — v0.3.0 language additions backfill.** Guide jumped
  straight from `.dty` stubs to "Putting it together" with no
  coverage of `newtype`, `freeze let`, or `pub`. Three new sections
  with emitted-Python comparisons and a `comptime` vs `freeze let`
  decision table. Salvaged from closed PR #102.
- **Roadmap reconciled with shipped reality.** Concrete-next-steps
  list updated: corpus round-trip sweep marked landed, loop
  parallelisation marked shipped (`tyc-analyse/src/parallel.rs`,
  opt-in via `[strictness] auto-parallel`), Salsa boundary entry
  notes that `preprocessed_text` / `resolved_module` /
  `check_diagnostics` / `module_shapes_query` are all
  `#[salsa::tracked]` (`check_file_with_imports` remains the
  multi-module bypass), `python_semantic_drift` "closed" set
  expanded with this release's work, native debugger remains
  unchanged-open.
- **`docs/diagnostics/arg_count.md` — inheritance section.**
  Rewrites the Limitations section to reflect the subclass-
  constructor fix; adds an Inheritance section recording the new
  walks-the-chain behaviour.
- **`docs/diagnostics/missing_field_init.md` — escapes-covered
  section.** Replaces the open-ended Limitations note with an
  explicit "Escapes covered" enumeration (return, call arg, container
  literal via annotated `let`, outer-scope alias via annotated
  `let`); intra-procedural + subclass caveats carried forward.
- **`examples/README.md`** gains a Language-additions row;
  `examples/48-...` comment corrected to reference
  `tyc::newtype_violation` (the dedicated diagnostic with wrap-fix
  help) instead of the generic `tyc::type_mismatch`.

## 0.3.1

A second stress campaign (`stress/round-2026-05-22/`, 93 fresh `.ty`
programs across nine domain buckets) filed thirteen new findings
(N1–N13, three of them silent-wrong-output CRITICAL). Every one of
them is closed in this release. Alongside that sweep: a new
`tyc::duplicate_method` diagnostic, typed tuple-unpacking on `let`,
a `tyc migrate` rewrite for the pre-PEP-695 `Generic[T]` idiom, a
`tyc run` correctness fix that gates VM execution behind a real
static check, and an LSP / `tyc check` polish pass.

This is a correctness-focused point release. No new headline language
features; the three CRITICAL fixes (N5, N6 emit parens, N9 VM `match`
scope) are the reason every adopter on 0.3.0 should upgrade.

### Added — language

- **Typed tuple-unpacking `let` (N4).**
  `let (a: int, b: str) = func(x, y)` now parses and desugars into a
  hidden `__typhon_unpack_N__` temp plus per-element `let` lines
  carrying the user-supplied annotations. Compound annotations
  (`list[int]`, `dict[str, int]`, `tuple[float, ...]`) survive the
  top-level-comma split, mixed captures (`let (a: int, b) = pair()`)
  emit the un-annotated leg with no type so inference fills it in,
  and the existing un-annotated `let (a, b) = pair()` form flows
  through unchanged.

### Added — diagnostics

- **`tyc::duplicate_method` (N8).** Two `impl Foo:` / `extend Foo:`
  blocks both defining `def get(self) -> …` used to merge silently,
  with Python keeping whichever definition desugar visited last. The
  new diagnostic anchors the second `def`, suggests rename / delete /
  merge, and runs after the type checker's class-shape merge so it
  doesn't double-report against `tyc::impl_unknown_class` for
  spurious `__typhon_impl_X` pseudo-classes.

### Fixed — emit / VM correctness (the three silent-wrong-output
finds)

- **`UnaryOp` paren wrap (N5, N6).** `not (a or b)` and
  `not (x if c else y)` no longer round-trip to the
  De-Morgan-violating `not a or b` / `not x if c else y` shapes.
  The `Expr::UnaryOp` printer arm now consults `expr_precedence` on
  the operand and wraps when the operand is lower-precedence
  (`Not` = 5, `Or` = 3, `And` = 4, `If` = 2, etc.). Surfaced
  organically against `05-agents/01-react-agent.ty` — a calculator-
  tool guard `not (ch.isdigit() or ch in " +-*/()")` was being
  emitted wrong.
- **VM `match` arm writes propagate to the outer scope (N9).** The
  tree-walking VM in `tyc-vm` ran each `case` arm in a fresh
  environment frame and discarded writes when the arm exited. Any
  `match` + accumulator pattern — every Result walker, every sealed-
  union aggregator, every state machine — saw `total = 0` from the
  VM and `total = 42` from `tyc run --compile`. The arm body now
  executes against the parent env, and pattern captures (`case
  Ok(v):` introducing `v`) are lifted into the parent env once the
  pattern + guard accept — matching CPython's `match` semantics,
  where bindings escape the arm. A failed pattern still discards
  any tentative captures so a partial bind on a non-matching arm
  never leaks.

### Fixed — `tyc run` / `tyc migrate`

- **`tyc run` gates the VM behind a static check (N10).** Previously
  the VM would happily evaluate a program with an unresolved name and
  crash with a Python-style `NameError`, hiding what would have been
  a clean `tyc::unknown_name` diagnostic. `tyc run` now runs
  `tyc check` first; only programs that type-check clean reach the
  VM.
- **`tyc migrate` rewrites `Generic[T]` → PEP 695 (N11).**
  Pre-3.12 generic idiom (`from typing import TypeVar, Generic`,
  `T = TypeVar("T")`, `class Box(Generic[T]):`) used to land in the
  output `.ty` unchanged, then trip
  `tyc::typevar_import_rejected` and the `Generic[T]` rejection on
  the very next `tyc build`. The rewriter now drops module-level
  `T = TypeVar(...)` declarations (including bounded /
  `constraints=` forms), rewrites `class X(Generic[T]):` into
  `class X[T]:` (multi-parameter, mixed-base, and qualified
  `typing.Generic` forms covered), and elides the now-dead `TypeVar`
  / `Generic` imports.

### Fixed — type-checker / desugar / emit

- **`freeze let` multi-line RHS (N1).** Multi-line dict and list
  literals on a `freeze let` no longer leak out of the synthesised
  `__typhon_freeze__(...)` call; the wrap now happens at the AST
  level, not as a text-level fix-up on the binding's first line.
- **`?` inside a comprehension fires a targeted diagnostic (N2).**
  `[parse(s)? for s in items]` used to silently hoist past the
  `for`-binding into a top-level `try`/early-return — semantics that
  no user wanted. The pre-pass now rejects `?` inside any kind of
  comprehension with a dedicated message ("`?` cannot be lifted out
  of a comprehension — rebind the result and unwrap it"); the
  outside-comprehension inline-`?` work (O17) keeps working.
- **Exhaustiveness on `match self.<field>:` (N13).** The
  exhaustiveness pass only inspected `Expr::Name` subjects when
  resolving the static type of a `match` subject. A class with a
  sealed-union field doing `match self.state:` therefore landed in
  the "subject type unknown" branch and the missing-return analysis
  treated the whole match as a potential fall-through (false-
  positive `tyc::missing_return` over a total match). The subject-
  type resolver now delegates `Expr::Attribute` to
  `infer_expr_readonly`. Non-exhaustive variants still surface both
  `tyc::non_exhaustive_match` and `tyc::missing_return`, as before.
- **`tyc::newtype_violation` covers boundary mismatches (N7).** A
  bare `int` flowing into a `UserId`-typed parameter used to fire
  `tyc::type_mismatch` with help text saying "expected UserId, found
  int" — the wrong-direction advice for a nominal alias, since the
  fix is `UserId(x)`, not "annotate as int." The newtype boundary is
  now routed through `tyc::newtype_violation` with help text that
  names the constructor call.
- **`from __future__ import annotations` not duplicated (N12).** A
  user who hand-wrote `from __future__ import annotations` at the
  top of a `.ty` file used to get a second one inserted by the emit
  pass, which Python's compiler then warned about and tools like
  `ruff format` would re-collapse on every save.
- **Comptime `str.join(...)` (N3).** Added to the comptime sandbox
  alongside the existing `str` / `int` / `float` / arithmetic /
  ternary / `env` surface. `comptime let URL: str = "/".join([host,
  path])` now evaluates at build time.

### Performance / tooling

- **Batched venv introspection + shared preprocess across check
  passes.** The third-party signature recovery introduced in 0.2.2
  used to fire one Python subprocess per module per `tyc check`
  invocation. The new batch path collects every public class /
  function across every imported module in one round-trip, and the
  preprocess output (rewrites for `let`, `pub`, `freeze`, …) is now
  shared across the resolver, the type-checker, the analyser, and
  the desugar pass via a Salsa-tracked query — a meaningful drop in
  end-to-end check time on every project that touches more than a
  couple of dependencies.

### LSP / `tyc check`

- **Semantic-token kinds for attribute access.** Bare-import access
  on a third-party class — `nn.Module`, `pd.DataFrame`,
  `torch.optim.Adam` — now paints as a class instead of falling
  through to the generic `property` / `method` token. The LSP
  introspects the receiver module's `dir(...)` once via the existing
  venv-signature cache and consults a `(receiver, attr) → kind` map
  before emitting each `Attribute` token.
- **Grouped `tyc check` diagnostics.** Errors are now grouped by
  source file (`-- errors in ./src/a.ty --`) instead of interleaved
  by analysis phase, with a per-code summary tally at the bottom
  (`1 error(s): tyc::arg_count`, `2 error(s): tyc::type_mismatch`)
  and an `tyc explain <code>` suggestion. CI logs cluster related
  errors instead of scattering them across files.
- **Surface introspection failure reasons in hover.** Hovering a
  third-party import whose subprocess introspection failed used to
  silently fall through to "no docs available." Hover now renders
  the actual reason (`NoPython`, `ImportFailed`, `Timeout`,
  `SpawnFailed`) and the recovery hint (`Install it with
  \`tyc add torch\`.`).
- **Per-module introspection timeout 3 s → 10 s** so heavy ML
  packages (`torch.nn` triggers C-extension init in the multi-
  hundred-ms range) complete on cold first-import instead of
  timing out.
- **Prewarm the actual dotted module path.** `import torch.nn as nn`
  now warms `torch.nn`, not just `torch`, so the first `nn.<dot>`
  doesn't block on the subprocess.
- **VS Code grammar.** The bundled TextMate grammar gains the v0.3.0
  keywords (`freeze`, `newtype`, `pub`, `frozen`, `plain`, `class!`),
  highlights `newtype X = Base` declarations alongside the existing
  type-alias rule, and accepts stacked modifier chains (`pub freeze
  let`, `pub comptime let`).

### Stress round 2026-05-22 (recorded in `docs/findings.md`)

- 93 fresh `.ty` programs across `01-language-edge` (incl. paren-
  precedence + freeze + match), `02-io`, `03-ml-numpy`,
  `04-ai-llm`, `05-agents`, `06-api`, `07-sdk`, `08-meta-stress`,
  `09-error-quality`.
- 13 findings filed (3 CRITICAL silent-wrong-output, 5 HIGH,
  2 MEDIUM, 3 LOW). All closed in this revision with a regression
  test plus the original stress probe retained as a forward-looking
  guard.

## 0.3.0

Six new language features for everyday Python annoyances, a coordinated
sweep that closes every remaining open finding from the May 2026 stress
campaigns (O2–O29), and cross-platform install support: pre-built `tyc`
binaries for **Linux (x86_64 + aarch64)** and **Windows (x86_64)** join
the existing **macOS (Apple Silicon + Intel)** matrix.

### Added — language

- **`newtype Name = Base` — nominal aliases over base types.**
  TypeScript-style nominal aliasing that keeps same-shaped primitives
  (`UserId` vs `PostId`, `USD` vs `EUR`, internal vs external IDs)
  from being silently swapped. Asymmetric by design: a `UserId` flows
  into an `int`-typed slot (the runtime value *is* an `int`), but a
  bare `int` requires an explicit `UserId(x)` constructor call to
  satisfy a `UserId`-typed target. Compiles to a zero-cost
  `typing.NewType` call. New `tyc::newtype_violation` diagnostic.
- **`freeze let X = expr` — deep-immutable bindings.** Closes the gap
  left by `let`, which only locks the binding name and not the
  underlying value. `freeze let` wraps the RHS in
  `__typhon_freeze__(...)` from `typhon_runtime.freeze`, which
  recursively converts `list → tuple`, `dict → MappingProxyType`,
  `set → frozenset`, and descends into nested values. Anything without
  a clean immutable equivalent (file handles, sockets, generators,
  non-frozen dataclasses) raises `TypeError` at startup rather than
  via a confusing downstream mutation.
- **`pub` modifier for module-level visibility.** When a module
  declares at least one `pub` name, desugar synthesises a top-of-file
  `__all__ = [...]` list so `from foo import *`, Sphinx autoapi, IDE
  re-export filters, and the checker's re-export inference all see
  the public surface — no hand-maintained `__all__` lists required.
  Composes with the existing keyword stack (`pub let`,
  `pub frozen class`, `pub model`).

### Added — diagnostics

- **`tyc::blocking_in_async`.** Catches direct calls to known-blocking
  stdlib functions (`time.sleep`, `requests.get`, `socket.recv`,
  `subprocess.run`, `input`, `urllib.request.urlopen`, …) inside an
  `async def` body. A blocking call halts the entire event loop until
  it returns; the diagnostic suggests `asyncio.to_thread(...)` /
  `loop.run_in_executor(...)`. Suppressed inside `unsafe:` regions.
- **`tyc::resource_not_managed`.** Flags bare assignments of
  context-manager-returning calls (`open`, `socket.socket`,
  `sqlite3.connect`, `tempfile.*`) that aren't wrapped in a `with`
  statement, where deterministic cleanup matters and the runtime
  would otherwise leave teardown to the garbage collector. Severity
  defaults to `warn`; controlled by `[strictness] resource-not-managed`.
- **`tyc::div_by_zero_literal`.** Catches `x / 0`, `x // 0`, and
  `x % 0` at compile time when the divisor is a literal — `0`, `0.0`,
  `-0`, `-0.0`, or any unary-negated form. Pure constant-fold lint
  with zero false positives. Flow-sensitive analysis (`if d != 0:`
  guards on runtime values) is deliberately out of scope.
- **`tyc::unsafe_value_leak`** (O14). A `return x` outside the
  `unsafe:` block where `x` was declared, against a function with a
  concrete annotated return type, now fires a dedicated diagnostic
  with help text pointing at both workaround forms (`let x: T = …`
  inside the block, or `let typed: T = x` outside).
- **`tyc::pattern_shadows_outer`** (O10). `case Wrap(value):` against
  an outer `let value` now fires a dedicated diagnostic instead of
  the misleading `tyc::immutable_assign`. Suggests renaming the
  capture — the right advice for the Rust/OCaml/Scala intuition every
  newcomer brings to `match`.
- **`tyc::extend_builtin`** (O23). `extend list[int]:` (parametric
  target) now fires a dedicated diagnostic naming the parametric
  shape, rather than the confusing downstream
  `tyc::impl_unknown_class` cascade.

### Added — install + release

- **Linux pre-built binaries.** `tyc-<version>-x86_64-unknown-linux-gnu.tar.gz`
  and `tyc-<version>-aarch64-unknown-linux-gnu.tar.gz` ship on every
  release tag. The same `install.sh` now detects `Linux` from
  `uname -s` and resolves the matching tarball; the macOS path is
  unchanged (Gatekeeper quarantine xattr clearing is skipped on
  Linux). Built on `ubuntu-22.04` for broad glibc compatibility.
- **Windows pre-built binaries.** `tyc-<version>-x86_64-pc-windows-msvc.zip`
  ships on every release tag, alongside a new `install.ps1`
  PowerShell installer. The script downloads the zip, verifies its
  SHA-256, extracts to `%LOCALAPPDATA%\Programs\Typhon` by default,
  and adds the directory to the user-level `PATH` via
  `[Environment]::SetEnvironmentVariable`. Supports `--Version` and
  `--InstallDir` flags + `TYPHON_VERSION` / `TYPHON_INSTALL_DIR`
  env vars.
- **Release workflow.** `.github/workflows/release.yml` now runs a
  five-job matrix (macOS Apple Silicon + Intel, Linux x86_64 +
  aarch64, Windows x86_64) and uploads tarballs + the Windows zip +
  a combined `SHA256SUMS` file to the GitHub Release. Linux
  aarch64 cross-compiles from Ubuntu via the `aarch64-linux-gnu-gcc`
  toolchain.

### Findings sweep — every open finding closed (O2–O29)

This release coordinates the two final findings-closure branches
(`claude/findings-documentation-review-HhuVH` and
`claude/finish-open-findings-dmLv6`). Across nine stress campaigns
(May 17–21 2026, ~600 hand-written `.ty` programs, ~120 distinct
findings) the **Open** column on `docs/findings.md` is now empty
for the first time since campaign tracking began.

- **`tyc fmt`** (O12, B9) — five new PEP 8 rules wired into the
  in-process pass: space after `:`, spaces around `->`, space after
  `,`, single-space around binary `+` / `-` and top-level `=`, two
  blank lines before top-level `def` / `class` / `async def`. The
  O12 repro `def    f(  x:int,y:int)->int:` reformats end-to-end.
- **TypedDict-style dict literals** (O15). `let alice: User = {"id":
  1, "name": "Alice"}` against a registered class shape now matches
  keys against fields and flows each value under the declared field
  type before falling through to the ordinary `dict[K, V]` path.
- **Sized-style Protocols on built-ins** (O16). `list`, `dict`,
  `tuple`, `set`, `str`, `bytes`, `range`, `frozenset`, `bytearray`
  now satisfy a user-declared Protocol whose declared methods are
  all common dunders (`__len__`, `__iter__`, `__getitem__`,
  `__contains__`, `__eq__`, `__bool__`, …).
- **Inline `?`** (O17). `Ok(add(parse(s)?, parse(t)?))` compiles. A
  new `expand_inline_question_ops` pre-pass lifts every mid-line `)?`
  into a `__typhon_qi_N__` temp + propagation guard before the
  existing end-of-line pass runs.
- **`while` narrowing** (O2). `Stmt::While` now applies test-implied
  narrowings to the body via the same path the `if` checker uses;
  the linked-list iterator idiom
  `while cur is not None: total += cur.value; cur = cur.next`
  type-checks.
- **`tuple[T, ...]`** (O3) — resolves to an internal
  `tuple_variadic[T]` head that the unifier accepts against any
  fixed-length tuple literal whose elements are all assignable to
  `T`, including `()`.
- **Cyclic type aliases** (O4) — surface `tyc::cyclic_type_alias`
  once, then rewrite the alias body to `Any` so subsequent uses fall
  through silently instead of cascading into `type_mismatch` errors.
- **`class! Foo(Exception)`** (O5) — synthesises
  `def __init__(self, *args, **kwargs)` calling `super().__init__`
  when the body has no annotated fields, so `raise AppError("boom")`
  reaches the parent constructor.
- **Generator `return`** (O6) — `return` and `return value` inside a
  generator function body type-check against the declared
  `Iterator[T]` / `Generator[Y, S, R]` return type.
- **`tyc migrate` nullable forward-refs** (O21) — `Optional["Item"]`
  now becomes `"Item?"`, not the previously unparseable `"Item"?`.
- **`tyc migrate` Union → `T?`** (O22) — rewrites `Union[T, None]`
  (and `Union[None, T]`, and the `typing.Union[...]` qualified form)
  to `T?` and drops the import; PEP 604 pipe-union fallback for
  multi-arm unions so imports are never left dangling.
- **`tyc explain --list`** (O25) — now prints every diagnostic code
  the binary knows about; the "not yet implemented" message for
  unknown codes is gone.
- **VM `Result` repr** (O24) — `Value::ResultOk` / `Value::ResultErr`
  match CPython's dataclass default (`Ok(value=20)` /
  `Err(error='oops')`); single-quote preference matches Python's
  `repr`. `tyc run` and `tyc run --compile` now produce
  byte-identical stdout for Result-bearing programs.
- **REPL prompts when piped** (O26) — `tyc repl` checks
  `stdin.is_terminal()` and skips the `>>> ` / `... ` prompts when
  stdin is piped.
- **`tyc build --no-sync`** (O29) — new flag (and `TYC_NO_SYNC=1`
  env var) skips the `uv sync` step while still merging
  `pyproject.toml`. Stress harnesses and REPL-like iteration on
  tmp projects no longer pay the per-invocation reprovision cost.
- **`MatchSequence` bracket recovery** (O27) — `case [a, b]:`
  re-emits as `[a, b]` rather than the default `(a, b)` by peeking
  at the original `TextRange` to recover the bracket choice.
- **Pipe corner cases** (O28) — `5 |> (lambda x: x * 2)()` and
  `(1 |> add(2)) |> add(3)` both compile. New
  `expand_pipes_in_subexpressions` pre-pass recursively expands
  pipes inside every balanced `(...)` group before the line-level
  pass runs.

### Tests

- 4 new preprocess unit tests for `newtype` lowering (`Name =
  NewType(...)` round-trip, generic bases, fmt round-trip,
  indented-form rejection).
- 7 new unit tests for `tyc::div_by_zero_literal` covering `/`,
  `//`, `%`, float zero, negated zero, and `unsafe:` suppression.
- New regression tests for `tyc::blocking_in_async`,
  `tyc::resource_not_managed`, `tyc::unsafe_value_leak`,
  `tyc::pattern_shadows_outer`, and `tyc::extend_builtin`.
- `tyc fmt` regression suite covers each of the five new PEP 8
  rules end-to-end.
- All B9-bucket pre-existing-bug repros under
  `stress/round-2026-05-21/` build clean.

## 0.2.5

Second editor-UX pass on the 0.2.4 LSP work. Closes the three
remaining "doesn't match what VS Code does for Python" gaps in
the original report:

### Added

- **Module-path tokens in `from X.Y import Z`.** Dotted module
  paths in both `from` and bare `import` statements now emit a
  `namespace` token per segment, with `defaultLibrary` applied for
  stdlib roots. Previously the resolver only emitted a token for
  `Z` (the binding), leaving `X.Y` uncoloured — visually the most
  prominent part of every import line. Each segment is its own
  token so themes (and future "go to import" code actions) can
  target sub-segments.
- **Shape-aware kwarg colouring.** Call sites like
  `Agent(client=…, model=…)` now classify each kwarg name against
  the callee's real signature:
  - `property` (orange) when the kwarg names a declared parameter.
  - `parameter` (yellow) when the callee declares `**kwargs` and
    the kwarg isn't on the explicit list.
  - No token (white) when the kwarg is unrecognised and the callee
    has no catch-all — a visible cue that something's off without
    us claiming a hard diagnostic.

  The LSP pre-resolves callee signatures by batch-querying the
  introspection cache for every top-level imported class /
  function in the open buffer. Signature parsing is tolerant of
  nested-tuple defaults (`a=(1, 2)`), quoted commas
  (`sep=', '`), positional-only `/`, kw-only `*`, and the
  conventional `self` / `cls` first parameter.
- **Class docstring fallback to `__init__`.** When a class body
  carries no docstring (common pattern when the author wrote the
  documentation on `__init__` instead), hover now falls back to
  `__init__.__doc__` so the popover still shows parameter
  documentation. Without the fallback the hover for classes like
  `agent_framework.Agent` degraded to "📦 from …" with no Markdown
  body.
- **RST inline cleanup in hover docstrings.** Two of the most
  common Sphinx markup shapes are normalised to Markdown before
  rendering: ``\`\`code\`\``` (RST double-backtick) becomes
  ``\`code\``` (Markdown), and Sphinx role markup like
  `:class:\`Foo\``, `:func:\`bar\``, `:meth:\`baz\`` has the role
  stripped, leaving just the backticked symbol. Pylance does the
  full Sphinx → HTML render; this is the 80/20 that catches the
  shapes appearing in most third-party docstrings.
- **Signature truncation cap raised** from 400 → 1024 characters
  so Pydantic / SQLAlchemy / Django model constructors with 20+
  typed parameters render their full signature instead of being
  dropped entirely.

### Tests

- 10 new tests in `tyc-lsp::semantic`:
  - Module-path emission: `from foo.bar import Baz`,
    `from os.path import join` (stdlib modifier), dotted
    `import foo.bar.baz`.
  - Kwarg classification: real-param → `property`,
    `**kwargs` catch-all → `parameter`, unknown → no token.
  - Signature parser: basic param names, `**kwargs` detection,
    nested-default commas, `self` / `cls` skip.
- 2 new tests in `tyc-lsp` for `render_docstring`: RST
  double-backtick code, Sphinx role directive stripping.

## 0.2.4

Editor developer-UX pass: semantic-token colouring for the LSP and
structured docstring rendering in hover. The two together close the
gap users hit on their first day with Typhon — "which of these
imports is from my project vs the library?" and "what arguments
does this take and what do they mean?".

### Added

- **LSP semantic tokens.** `textDocument/semanticTokens/full` is now
  served. Walks the resolved module and parsed AST to emit a token
  stream tagged with LSP-standard types (`class`, `function`,
  `method`, `property`, `parameter`, `variable`, `namespace`) and
  modifiers (`declaration`, `defaultLibrary`). VS Code themes apply
  colours automatically — stdlib imports get the `defaultLibrary`
  shade (Python convention: muted blue), third-party / project
  imports get the usual class/function colours, and method calls
  (`agent.run()`) are coloured distinctly from property reads
  (`agent.name`). The legend is published in the server capabilities
  so the indices stay stable across releases.
- **Structured docstring sections in hover.** `render_docstring` now
  detects Google / NumPy / Sphinx-style sections (`Args:`,
  `Parameters\n----------`, `:param name:`) and re-renders them as
  Markdown headers + bullets. Parameter lines (`name: desc`,
  `name (type): desc`, `name : type\n    desc`) become
  `- **name** — desc` bullets, so the hover popover shows the
  parameter list with descriptions instead of a wall of indented
  text. Free prose, examples, and unrecognised sections are
  preserved verbatim.

### Tests

- 7 new tests in `tyc-lsp::semantic` covering stdlib /
  third-party / project token classification, declaration-modifier
  emission on class + function decl sites, method-vs-property
  detection in `Expr::Attribute`, and LSP delta-encoding
  correctness.
- 5 new tests in `tyc-lsp` for `render_docstring`: Google `Args:`,
  NumPy `Parameters\n----------`, Sphinx `:param X:`, recognised
  `Examples:` section, and the pass-through for unstructured
  prose. The existing PEP 257 indent-strip test is updated to
  assert on the new structured output.

## 0.2.3

UX polish on the arity diagnostic landed in 0.2.2 plus full hover
developer-UX for third-party imports in the LSP. Lands as one
release: the diagnostic naming and the hover preview are the same
"tell me what's wrong / tell me what this thing is" story.

### Added

- **`tyc::missing_argument` diagnostic.** When the checker can
  pinpoint *which* parameter wasn't supplied (constructor or
  free function), the new code fires instead of the count-based
  `tyc::arg_count` — so `Agent(name=..., tools=...)` now reads
  `missing required argument to 'Agent': 'client'` and a one-line
  `help: supply 'client' when calling 'Agent'`, instead of the
  misleading "expected 1, got 4". Multiple missing names render as
  ``` `a`, `b` ``` with the plural form. `tyc::arg_count` still
  fires for shape mismatches that can't be reduced to a missing-name
  list — `missing_required_fields` and `missing_required_params`
  detect positional+kwarg double-binding, too-many-positionals, and
  `*iter` / `**dict` unpacks and return empty so the count-based
  diagnostic stays the source of truth for those cases.
- **LSP hover docs for third-party imports.** Hovering an imported
  class or function in VS Code (or any LSP-aware editor) now shows
  the source module, the recovered signature in a fenced `python`
  code block, and the runtime docstring rendered as proper Markdown
  — pulled from the same venv-introspection cache that already
  powers completion. Project / stdlib symbols fall through to the
  existing kind-only hover.
- **Responsive prewarm on document open / change.**
  `check_and_publish` spawns a detached task that introspects every
  third-party import in the open buffer the moment the file is
  parsed, so the cache is hot by the time the user hovers. The
  first hover used to wait on the subprocess (30–100 ms typical);
  subsequent hovers hit the cache. The prewarm never blocks
  diagnostics publishing, and is debounced by document version so
  rapid typing doesn't queue redundant blocking-thread work.
- **Markdown rendering for hover.** Hover now publishes
  `MarkupContent { kind: Markdown, … }` instead of the older
  `MarkedString::String` shape so signatures appear in fenced
  code blocks and docstrings render as paragraphs.
- **Multi-line docstrings.** The Python introspection script now
  returns the full docstring (up to 4 KB) instead of the first
  line; the LSP strips PEP 257 indentation and trims surrounding
  blanks before rendering. Caps the visible body at 40 lines with
  an explicit truncation marker so module-level docstrings (numpy,
  pandas, sklearn) don't flood the popover.
- **Off-runtime introspection in hover.** The hover handler runs
  the cold-path `cache.members()` call through
  `tokio::task::spawn_blocking` so a worst-case 5-second timeout
  can never stall the async runtime. The prewarm makes cold hits
  rare; this is the safety net for the case where the prewarm
  hasn't completed yet.
- **Poisoned-mutex recovery on hover + prewarm.** Both paths now
  recover poisoned `IntrospectionCache` mutexes via
  `into_inner()`, matching the completion path. A prior panic no
  longer permanently disables hover / prewarm for the rest of the
  session.

### Tests

- The three `tyc-db` cross-module tests and the four `tyc-types`
  arity tests that asserted on "wrong number of arguments" now
  match the new wording (and assert on the specific missing name).
- 4 new regression tests in `tyc-types` covering the
  `missing_argument` guards: positional+kwarg double-binding (ctor
  + free fn), too-many-positionals (ctor + free fn with kw-only
  required).
- 5 new tests in `tyc-lsp` covering `render_docstring` (PEP 257
  strip, blank-line trim, 40-line cap, empty input) and `sig_tail`
  (prefix strip, unknown-shape pass-through).
- One `tyc-types` arity test on `def add(a, b)` accepts either
  wording to stay forward-compatible with future diagnostic
  refinements.

## 0.2.2

Third-party signature recovery via venv introspection. The flagship
gap this release closes: a project that imports a class from an
unstubbed PyPI package (`from agent_framework import Agent`) and
calls it missing a required argument (`Agent(name="x", tools=[])`
without the required `client` kwarg) passed `tyc check` and
`tyc build` clean in 0.2.1, then crashed at runtime with
`TypeError: Agent.__init__() missing 1 required positional
argument: 'client'`. The checker had no signature for `Agent`
because no `.dty` stub was authored, so the callable degraded to
`Type::Unknown` and the arity check at
`tyc/crates/tyc-types/src/lib.rs:6061` (which only fires for
project-local classes) was skipped.

0.2.2 closes the loop by shelling to the project's
`.venv/bin/python` (or a fallback `python3` on PATH), asking
`inspect.signature` for the real parameter list of every public
class and free function in each declared dependency, and folding
the result into the same `ModuleShapes` registry that
`tyc-db::build_external_shapes` already consumes. No changes to the
checker itself — once the shape is in the registry, the existing
cross-module constructor / function arity path fires identically
to in-project calls.

### Added

- **`tyc/src/venv_signatures.rs`: venv-driven signature
  introspection for the type checker.** Walks every `.ty` file's
  `import` / `from ... import ...` / `lazy import` statements to
  collect dotted module names, then runs an embedded Python helper
  in the project venv that emits structured per-parameter info
  (name, kind, has-default) for every public class and free
  function. The Rust side converts that into an `InterfaceShape`
  (for classes — each `__init__` param becomes a field, with
  defaulted params populating `field_defaults`) or an `ArityInfo`
  (for free functions — kw-only / positional / `*args` / `**kwargs`
  preserved), and merges the result into the project shape
  registry. `tyc check` and `tyc build` consume the enriched
  registry; no changes were needed in `tyc-types` or `tyc-db`.

### Safety rails

- Only modules whose top-level package is listed in
  `[dependencies]` / `[dev-dependencies]` (or maps to a declared
  distribution via `.dist-info/top_level.txt`) are introspected.
  Stdlib (`os`, `json`, `collections`) and project modules stay on
  their existing resolution paths — no Python subprocesses for
  ordinary stdlib usage.
- Classes whose `__init__` declares `*args` or `**kwargs` are
  deliberately skipped. False positives on permissive Python APIs
  (every extra kwarg firing `tyc::unknown_kwarg`) would be worse
  than the existing miss.
- All failures (no venv, no Python on PATH, import-time exception,
  5-second introspection timeout) silently no-op. Worst case is the
  prior 0.2.1 behaviour — the runtime catches what the checker
  couldn't.
- Subprocess `current_dir` is pinned to the project root so the
  same `tyc check` from any subdirectory produces the same shape
  registry. Same reproducibility contract that already applies to
  `tyc::unknown_module`.
- The introspection result is cached per `VenvSignatures` instance
  keyed by dotted module name; one subprocess per module per
  `tyc check` invocation.

### Tests

- 10 new unit tests in `tyc::venv_signatures` covering the
  shape-conversion logic (required kw-only params, `**kwargs`
  bail-out, `*args` bail-out, positional+kw-only mix, `*args` /
  `**kwargs` on free functions, dotted-name validation, allow-list
  gating, and import-statement extraction across every Typhon /
  Python form).
- One new integration test in `tyc::commands::check::tests`
  (`check_introspects_third_party_class_constructor_arity`) that
  builds a fake third-party package, runs the introspection
  helper, and asserts the recovered shape models the original bug.
  Skips silently when no Python 3 is on PATH so CI runners without
  Python don't fail the suite.

### Caveats

- An author who genuinely needs the missing-arity check on a
  class with `**kwargs` should write a `.dty` stub — the stub
  declares the real surface and the arity check fires normally.
  See `docs/guides/08-…-stubs` (TBD).
- Python imports can have side effects. Modules that touch the
  network or sleep at import time would now do so during
  `tyc check`. The 5-second timeout caps the cost; users on
  pathological packages can declare the dep out of
  `[dependencies]` to skip introspection.

## 0.2.1

Correctness fixes surfaced by the
`stress/round-2026-05-20-exploration` corpus (~80 fresh `.ty`
programs across syntax edges, I/O, ML/NumPy, AI/LLM clients, agents,
HTTP servers, and SDK clients).  Documented per finding under
`stress/round-2026-05-20-exploration/FINDINGS.md`.

The flagship fixes in this point release are two **silent
wrong-output / wrong-rejection** bugs that the prior round's stress
corpus shook out:

- A pathological hole in the emitter precedence table that let
  `(a + b).upper()` round-trip as `a + b.upper()` — same semantics
  problem as the walrus bug fixed in 0.2.0, but with attribute /
  call / subscript / boolean-op as the outer context instead of an
  arithmetic BinOp.  Surfaced live in `pathlib.Path / "x"` chains and
  a NumPy least-squares reduction (`(dx * dy).sum() / (dx * dx).sum()`).
- The type checker refused to accept `None` or `T?` values against an
  `impl X: def f(self, p: T?)` parameter — the method's per-param
  types were never recorded and fell back to a `Vec![Type::Unknown; arity]`
  shape, which collided with the call-site
  nullable-into-non-nullable guard.

### Fixed

- **`tyc-emit`: parentheses preserved around non-atomic expressions
  used as the receiver of postfix ops.**  `Attribute`, `Call`,
  `Subscript`, and `BoolOp` emit arms now route the inner expression
  through a `needs_paren_for_postfix` guard.  Adds wrapping around
  `BinOp` / `BoolOp` / `UnaryOp` / `Lambda` / `IfExp` / `Compare` /
  `Named` (walrus) / `Await` / `Yield` / `YieldFrom` / `Starred` /
  `Generator` whenever they appear in those four contexts.  Also
  parenthesises a lower-precedence `BoolOp::Or` child inside a
  `BoolOp::And` parent.  (FINDINGS E1 — silent precedence bug.)
- **`tyc-types`: `MethodSig` now records per-parameter types.**
  `collect_class_shape` populates `MethodSig::param_types` from the
  declared annotations (stripping the implicit `self` / `cls` slot
  to mirror `arity_info`).  The call-site resolution path for
  `instance.method(...)` reads these into the `Type::Function::params`
  it returns, so `instance.method(value)` is now type-checked against
  the real declared parameter types instead of `Vec![Type::Unknown; arity]`.
  Fixes `impl X: def f(self, p: T?)` rejecting `None` and `T?`
  arguments.  (FINDINGS E2 — impl `T?` params.)
- **`tyc-types`: `self` inside an `impl` block carries the user-facing
  class name, not the desugarer's `__typhon_impl_X` pseudo-class.**
  The pseudo-class is an intermediate shape that the merge pass folds
  into the real class; previously the checker walked the methods
  first and `return self` against `-> X` failed with
  `expected X, found __typhon_impl_X`.  Stripping the prefix at
  `self`-receiver binding time gives the user the diagnostic surface
  they expect.  (FINDINGS E3 — broke `__enter__` / context-manager
  patterns.)
- **`tyc-types`: call-site `func_type` is unwrapped through type
  aliases.**  Calling through a transparent
  `type Handler = Callable[[Req], Resp]` alias now resolves to the
  underlying `Type::Function`, so the call's return type is `Resp`
  rather than `Handler`.  Middleware / decorator / handler-pipeline
  patterns work without inlining the `Callable[...]`.  (FINDINGS E4.)
- **`tyc-types`: user-defined dunders take precedence over the
  numeric-coercion table in `BinOp` inference.**  `Vec2(...) * 5.0`
  for `impl Vec2: def __mul__(self, scalar: float) -> Vec2` now
  resolves to `Vec2`, not `Float`.  Also reaches for the reflected
  dunder (`__radd__` / `__rmul__` / …) when the LHS is a primitive
  and the RHS is a user class.  (FINDINGS E5.)
- **`tyc-types`: exhaustiveness recognises positional class
  patterns.**  `case Leaf(value):` / `case Branch(left, right):`
  (positional captures of every declared field) now count as total
  matches for the named variant, so `missing_return` no longer fires
  on legitimately-total `match` statements over recursive sealed
  unions.  (FINDINGS E6 / R3.12 follow-up.)
- **`tyc-analyse`: comptime f-strings.**  `comptime let TITLE: str = f"{APP} v{MAJOR}.{MINOR}"`
  now evaluates at build time as long as every interpolation is
  itself a comptime constant.  Format specs and conversion flags
  (`!r` / `!s` / `!a` / `:>5`) remain unsupported and surface as
  explicit comptime errors instead of being silently dropped.
  (FINDINGS E7.)
- **`tyc-types`: calling a `Type::Class(...)` whose shape we don't
  have degrades to `Unknown` instead of treating it as a
  constructor.**  Catches the `self.linear(x)` shape where `linear`
  is an instance field typed as a foreign class (e.g. `torch.nn.Linear`) —
  the call is most likely invoking `__call__`, not constructing a
  fresh instance.  Pre-fix the imported class's "constructor" result
  would leak into `mut`-bound rebinds; post-fix the result is
  `Unknown` and the surrounding annotation drives assignability.
  (Knock-on fix surfaced by E3.)

### Tests

- 7 new regression tests in `tyc-types` covering E2, E3, E4, E5,
  E6 (positional / shorter-positional class patterns), and the
  dunder-rejects-arg-type-mismatch case added during PR-#87 review.
- 3 new regression tests in `tyc-analyse` covering E7 (the happy
  path, the format-spec rejection, and the
  list/tuple/dict-interpolation rejection added in PR-#87 review).
- 6 new emit round-trip tests in `tyc-emit` covering the
  paren-stripping shapes (`(BinOp).attr`, `(BinOp)[idx]`,
  `(ternary).attr`, `(lambda)(arg)`, `(Or).And`,
  `(path / "x").method(...)`).

### Documented in `stress/round-2026-05-20-exploration/`

A new stress round runs the full eight-domain corpus against this
release.  The remaining open issues (E8 class-const slot
descriptors, E9 `?` in subexpressions, E10 `for`-target rebinding,
E11 per-build venv DX, and the typed-SDK `dict[str, object]`
variance issue) are catalogued in
`stress/round-2026-05-20-exploration/FINDINGS.md` for the next
round.

## 0.2.0

Constructor / method arity safety. The flagship bug this release
catches: a class declared with `class ApiClient: api_key: str` that
the user instantiates as `ApiClient(base_url="…")` — passing `tyc
check` and `tyc build` in 0.1.6, crashing at runtime with `TypeError:
missing 1 required positional argument`. v0.2.0 surfaces the same
bug at check time, before the build ever runs.

### Review-driven fixes (post initial v0.2.0 RC)

- Bare-import dotted access now uses qualified `module.class` names
  internally, so two imports exporting the same class don't collide
  in the lookup table. Diagnostics also surface the qualified name
  (`clients.ApiClient`) for disambiguation.
- Same fix applied to imported free functions.
- `model X:` declarations correctly require every non-defaulted
  field at construction even when defaults appear earlier in the
  body — `ArityInfo` now carries a per-param `required_positional`
  flag rather than relying on the "all required come first" Python
  convention.
- `impl X:` / `extend X:` blocks contributing fields with defaults
  now merge `field_defaults` correctly; previously they were treated
  as required at construction.
- `[project] src = "app/src"` (nested src) now derives dotted names
  correctly — the basename of the configured src dir is what
  `path_to_dotted` actually needs.
- The post-construction audit no longer fires on `return c.field` or
  `f(c.field)` (receiver-of-attribute-access isn't an instance
  escape) and no longer emits duplicate diagnostics for repeated
  names like `return c if cond else c`.
- `setattr(obj=c, name=…, value=…)` (kwarg form) now drops audit
  tracking like the positional form does.
- `infer_expr_readonly` handles `Expr::Call` so chained calls like
  `clients.ApiClient(…).url(…)` resolve the receiver type for
  method arity checks.
- The project shape registry is now `Arc<HashMap<…>>` end-to-end so
  per-file `ExternalShapes` snapshots are O(1) refcount bumps
  instead of O(modules) deep clones.

### Added

- **`tyc::arg_count` now fires on class constructors.** The
  auto-generated `__init__` of `class` and `model` declarations is
  arity-checked at every call site. Fields with no `= default` are
  required; fields with a default are optional. `T?` without an
  explicit `= None` is still required (Typhon doesn't auto-inject the
  default), matching the emitted dataclass's runtime semantics.
- **`tyc::arg_count` now fires on `impl` methods.** Method signatures
  carry full `ArityInfo` (param names, defaults, `*args`/`**kwargs`)
  instead of a single arity count. `user.greet()` is flagged when
  `greet` declares a required `prefix: str` parameter.
- **Cross-module arity checks.** `from foo import ApiClient`
  followed by `ApiClient(…)` arity-checks against `foo`'s exported
  shape. `import foo as f` followed by `f.ApiClient(…)` does too via
  the new `Type::Module(name)` modelling. Both `.ty` source and
  `.dty` stubs flow through a project-wide shape registry built once
  per invocation. Works in `tyc check`, `tyc build`, and the LSP.
- **Salsa-cached shape extraction.** A new
  `tyc_db::module_shapes_query(file)` salsa-tracked query caches
  per-file shape extraction. The LSP backend keeps a per-project
  `HashMap<dotted_name, SourceFile>` so handles survive across
  keystrokes; a keystroke in one file only re-runs extraction for
  that file.
- **`tyc::missing_field_init` post-construction audit.** Catches the
  `X.__new__(X)` / `object.__new__(X)` bypass-construction pattern:
  if the instance escapes the function (return / call argument)
  without every required field assigned, the audit fires. Dropped
  conservatively on `setattr`, on `obj.method(…)` calls, and inside
  `unsafe:` regions.
- Public API in `tyc-types`: `InterfaceShape`, `MethodSig`,
  `ArityInfo`, `ModuleShapes`, `ExternalShapes`, plus
  `extract_module_shapes` and `check_module_with_imports`. Downstream
  tools building on the type-check pipeline now have a stable
  surface for cross-module checks.

### Changed

- `Type::Module(String)` joins the type enum, exposed for the bare-
  import attribute-access path.
- `tyc-db` re-exports `ModuleShapes` so CLI and LSP callers don't need
  to depend on `tyc-types` directly.
- The check pipeline's `Type::Function` arm now consults
  attribute-callee arity info before falling through to the
  permissive shape, closing the long-standing method-arity gap.

### Limitations

- Dotted-attribute annotations (`let c: f.Cls = …`) don't yet resolve
  to the foreign class shape. The constructor call itself
  arity-checks, but the binding lands as `Type::Unknown`. Workaround:
  use `from foo import Cls` or drop the annotation.
- The post-construction audit only flags return statements and
  function-call arguments as escapes; container-literal storage
  (`return [c]`) and outer-scope assignment aren't tracked.
- The audit is intra-procedural. A method that genuinely initialises
  fields suppresses the diagnostic (correctly); a method that
  doesn't will also suppress it (false negative).
- The audit doesn't track subclass field requirements separately.

### Migration notes from 0.1.6

The new arity checks are strict by default and may surface
pre-existing latent bugs in your codebase — that's the point. Two
patterns commonly need attention:

1. `Foo()` calls where `Foo` has required fields. Either add the
   missing arguments or give the fields defaults (`field: str = ""`).
2. Method calls that previously passed under the permissive shape.
   If a method signature changes (e.g. you add a parameter), every
   call site is now flagged immediately.

There are no behavioural changes at runtime — the emitted Python is
identical to 0.1.6. Only `tyc check` / `tyc build` reject more
programs.

## 0.1.6

See the [v0.1.6 release notes](https://github.com/CodeHalwell/Typhon/releases/tag/v0.1.6).

Phase 5 — Interop and developer experience: `plain class` marker,
`Enum`/`Flag`/`ABC` auto-skip plus configurable
`[emit] skip-decoration-bases`, `class-default` validation,
`or`/`and` truthy-union typing, generator→`Iterable` conformance,
`tyc explain <code>` / `tyc cheatsheet`, upgraded `tyc init`,
`.py`-in-`src/` copy-through, `tyc build --check`,
`tyc::contains_secret_literal`, miette `url(...)` deep-links,
`tyc fmt` wrapping `ruff format`, `tyc debug --break <ty>:<line>`.

## Earlier

For history before 0.1.6 see `docs/roadmap.md` and the corresponding
GitHub release tags.
