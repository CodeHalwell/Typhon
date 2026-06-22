# Type System Frontier — Status

This document tracks the type-system frontier work: what has **landed** in the
development line, and what remains **open**. It supersedes the original
"foundation" summary from
[Epic: type-system frontier — HKT, full variance inference, comptime types-as-values](https://github.com/CodeHalwell/Typhon/pull/113);
the unification and variance-inference work that epic scoped as future has since
shipped (post-v0.15.7, on the line converging toward `v1.0.0-alpha`).

---

## Landed (post-v0.15.7, development line)

### 1. Higher-Kinded Types (HKT) — unification ✅

The HKT scaffold (the `Type::TypeConstructor` variant and `F[_]` parser support,
v0.5.0) is now backed by real unification.

- **Constructor-variable binding**: `bind_typevars_and_substitute` binds a
  constructor variable against a concrete head — `F[A]` against `list[int]`
  binds `F = list, A = int` — and substitutes `F`/`A` in the return type, so a
  `class Functor[F[_]]:` / `interface Functor[F[_]]:` with a `map` over `list`
  (and other single-arg constructors) type-checks.
- **Kind checking**: applying a constructor variable with the wrong arity
  (`F[A, B]` against `list[int]`), or binding one `F` to two different
  constructors within a single call, emits the new **`tyc::kind_mismatch`**
  diagnostic. Backward kind-error propagation and generic-impl-header remapping
  are wired through `tyc-diagnostics`.
- **Cross-module**: constructor identity propagates across module boundaries
  alongside variance (a `Functor[F[_]]` declared in one module unifies in a
  consumer).

**Still deferred** (see *Open* below): function-level HKT params
(`def f[F[_]](...)`), constructor application on non-class heads, and
constructor composition.

Code: `tyc/crates/tyc-types/src/lib.rs` (`Type::TypeConstructor`, `bind_typevars_and_substitute`),
`tyc/crates/tyc-diagnostics/src/lib.rs` (`kind_mismatch`).

### 2. User-generic variance inference ✅

User-declared generics are no longer invariant-by-default. A pass classifies
each class type-parameter's usage and stores per-class variance, consulted in
`is_assignable`.

- **Inference from usage**: a param used only in output position infers
  **covariant**, only in input position **contravariant**, in both **invariant**
  (`infer_class_param_variance` / `collect_param_variance`, composed via
  `compose_variance` / `join_variance`). So a `class Producer[T]` with `T` only
  in returns accepts `Producer[Dog]` where `Producer[Animal]` is expected.
- **Explicit override**: a bare `@covariant` / `@contravariant` class decorator
  forces the variance regardless of inferred usage (`explicit_variance_override`).
- **Through interface bounds (C4)**: variance flows soundly through
  bounded type-params (`T: SomeInterface`) — this closed a soundness hole, not
  just a relaxation.
- **Cross-module**: inferred variance propagates across module boundaries.

Built-in variance (mutable containers invariant; read-only views, `tuple`,
`Mapping` values, `Callable` return covariant; `Callable` args contravariant;
`Result` covariant in both) was already in place and is unchanged.

Code: `tyc/crates/tyc-types/src/lib.rs` (`Variance`, `generic_param_variance`,
`user_generic_param_variance`, `infer_class_param_variance`).

### 3. General inter-procedural field-init audit ✅

The trivial factory-helper special case is replaced by a per-function summary
that tracks partial-instance escapes across call chains, so a partially
initialised instance escaping through a non-trivial helper chain fires
`tyc::missing_field_init` (no false positives on the corpus).

Code: `tyc/crates/tyc-types` / `tyc/crates/tyc-analyse` (per-function init summary).

### 4. Comptime types-as-values ✅ (since v0.5.0)

`comptime let T: type = int` and the runtime-resolvable builtin type set
(`int`, `str`, `bool`, `float`, `bytes`, `None`, `type`, `object`) emit as bare
type expressions and are usable in annotation positions. `Any` is excluded (not
a runtime builtin). Unchanged from the original implementation.

Code: `tyc/crates/tyc-analyse/src/lib.rs` (`ComptimeValue::Type`).

---

## Open (remaining frontier)

These are the items the language reference and skill point here for:

- **Embedded `ty` (Phase 2)** — run Astral's `ty` in-process sharing the Salsa
  DB, eliminating the subprocess parse/re-elaborate round-trip
  (`docs/ty-integration.md` Phase 2). Blocked on vendoring `ty` (a git source)
  past the `cargo deny` supply-chain policy, then sharing the DB. The Phase 1
  subprocess path (`[checker] external = "ty"` / `--with-ty`) ships and remains
  the fallback; Phase 2 is **perf-only**, no new capability.
- **Typeshed-backed checking for pure-extension libraries** — arg-type checking
  for `.pyi`-only / C-extension packages (numpy/pandas public API) whose
  signatures venv introspection can't recover. Shares typeshed handling with the
  embedded-`ty` work.
- **Accumulator-loop parallelisation** — comprehension parallelisation already
  ships (`auto-parallel`); generalising it to accumulator loops is a non-goal
  for the alpha.
- **Function-level HKT params, non-class constructor application, constructor
  composition** — the deferred remainder of the HKT work above.

---

## References

- Alpha plan: [`docs/alpha-release-plan.md`](docs/alpha-release-plan.md) (WS-C, WS-D, WS-A/A3).
- `ty` integration: [`docs/ty-integration.md`](docs/ty-integration.md).
- Epic PR (original scope): [#113](https://github.com/CodeHalwell/Typhon/pull/113).
