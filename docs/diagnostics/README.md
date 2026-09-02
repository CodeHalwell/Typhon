# Typhon diagnostic codes

Every error or warning the Typhon compiler emits carries a stable diagnostic
code of the form `tyc::<short_code>`. Each code below links to a page
explaining when it fires, the underlying language rule, and how to fix it.

The same URL pattern is rendered next to the code in terminal output —
`https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/<short_code>.md` — so users can jump
straight from a failed build to the corresponding documentation.

## Codes

- [`tyc::arg_count`](./arg_count.md) — function, method, or class constructor called with the wrong number of arguments.
- [`tyc::async_without_await`](./async_without_await.md) — warning: `async def` body never uses `await`.
- [`tyc::attribute_not_found`](./attribute_not_found.md) — attribute access on a type that doesn't declare it.
- [`tyc::auto_gather_missed`](./auto_gather_missed.md) — advice: adjacent awaits look gatherable but a callee isn't `@gatherable`.
- [`tyc::blocking_in_async`](./blocking_in_async.md) — direct call to a known-blocking stdlib function (`time.sleep`, `requests.get`, `subprocess.run`, …) inside an `async def` body.
- [`tyc::class_attr_shadows_slot`](./class_attr_shadows_slot.md) — warning: defaulted field becomes a slot descriptor at runtime.
- [`tyc::comptime`](./comptime.md) — comptime expression failed to evaluate at build time.
- [`tyc::contains_secret_literal`](./contains_secret_literal.md) — warning: `comptime` binding inlines a secret-shaped env value.
- [`tyc::cyclic_type_alias`](./cyclic_type_alias.md) — `type` alias chain forms a cycle.
- [`tyc::div_by_zero_literal`](./div_by_zero_literal.md) — `/`, `//`, or `%` with a literal `0` (or `0.0`, `-0`, unary-negated zero) on the right.
- [`tyc::duplicate_class`](./duplicate_class.md) — same class name declared more than once at the same scope.
- [`tyc::duplicate_method`](./duplicate_method.md) — two `impl` / `extend` blocks on the same class both define a method with the same name.
- [`tyc::empty_collection_no_annotation`](./empty_collection_no_annotation.md) — warning: empty collection literal bound without a type annotation, so the element type cannot be inferred.
- [`tyc::extend_builtin`](./extend_builtin.md) — `extend` targets a Python built-in type.
- [`tyc::field_default_ordering`](./field_default_ordering.md) — class declares a non-defaulted field after a defaulted one; the synthesised `__init__` would raise at import time.
- [`tyc::freeze_not_freezable`](./freeze_not_freezable.md) — `freeze let X = <expr>` RHS constructs a non-`frozen` user class. Validated at check time since v0.9.0 (was a runtime `TypeError` before).
- [`tyc::frozen_assign`](./frozen_assign.md) — field assignment on a `frozen` class outside its constructor.
- [`tyc::frozen_inheritance_conflict`](./frozen_inheritance_conflict.md) — a dataclass and its base disagree on frozen-ness; CPython would raise `TypeError` at import.
- [`tyc::gather_opportunity`](./gather_opportunity.md) — advice: adjacent independent awaits could run concurrently in a `gather:` block.
- [`tyc::generator_return_type`](./generator_return_type.md) — body contains `yield` but the return type isn't iterator-shaped.
- [`tyc::generic`](./generic.md) — catch-all early-phase diagnostic.
- [`tyc::immutable_assign`](./immutable_assign.md) — re-assignment to a `let` binding.
- [`tyc::impl_unknown_class`](./impl_unknown_class.md) — `impl NAME:` targets a class not in scope.
- [`tyc::implicit_any`](./implicit_any.md) — bare collection annotation with implicit `Any` element type.
- [`tyc::impure_pure_fn`](./impure_pure_fn.md) — `@pure` function violates one of the purity conditions.
- [`tyc::incompatible_override`](./incompatible_override.md) — warning: subclass method overrides a base method with an incompatible signature.
- [`tyc::interface_isinstance`](./interface_isinstance.md) — `isinstance(x, Interface)` without `@runtime_checkable` opt-in.
- [`tyc::interface_not_conforming`](./interface_not_conforming.md) — value doesn't structurally conform to interface.
- [`tyc::invalid_config_value`](./invalid_config_value.md) — `typhon.toml` value outside the allowed enumeration.
- [`tyc::invalid_question_op`](./invalid_question_op.md) — `?` outside a `Result`-returning function, or inside a comprehension.
- [`tyc::io`](./io.md) — source file could not be read.
- [`tyc::is_literal_comparison`](./is_literal_comparison.md) — warning: `is` / `is not` against a literal compares identity, not value.
- [`tyc::kind_mismatch`](./kind_mismatch.md) — higher-kinded type constructor applied with the wrong arity or bound to conflicting constructors.
- [`tyc::lazy_import_opportunity`](./lazy_import_opportunity.md) — advice: heavy module imported eagerly but used rarely — a `lazy import` candidate.
- [`tyc::lazy_usage`](./lazy_usage.md) — unsupported form under the `lazy` keyword.
- [`tyc::loop_closure_capture`](./loop_closure_capture.md) — warning: closure created in a loop captures the loop variable by reference, not value.
- [`tyc::main_not_called`](./main_not_called.md) — advice: `def main()` defined but never called.
- [`tyc::manual_init`](./manual_init.md) — class body declared `__init__` directly.
- [`tyc::method_in_class_body`](./method_in_class_body.md) — `def` inside a `class` body instead of an `impl` block.
- [`tyc::missing_annotation`](./missing_annotation.md) — parameter or return type missing its annotation.
- [`tyc::missing_argument`](./missing_argument.md) — call site doesn't supply a specific required parameter (the named form of `arg_count`).
- [`tyc::missing_await`](./missing_await.md) — sync caller called an `async def` without awaiting it.
- [`tyc::missing_binding_kind`](./missing_binding_kind.md) — local binding missing `let` or `mut`.
- [`tyc::missing_field_init`](./missing_field_init.md) — `X.__new__(X)`-constructed instance escapes without every required field assigned.
- [`tyc::missing_initialiser`](./missing_initialiser.md) — `let`/`mut` declaration without an initialiser.
- [`tyc::missing_return`](./missing_return.md) — function path reaches end without a return value.
- [`tyc::mutable_default_param`](./mutable_default_param.md) — warning: mutable default parameter value is shared across calls.
- [`tyc::newtype_invalid_base`](./newtype_invalid_base.md) — `newtype` declared over something that is not a type (e.g. a literal).
- [`tyc::newtype_violation`](./newtype_violation.md) — wrong-typed argument passed to a `newtype` constructor (or a bare base value flowing where a newtype is expected).
- [`tyc::no_block_shadow`](./no_block_shadow.md) — inner `let`/`mut` would shadow a function-scoped outer binding.
- [`tyc::non_exhaustive_match`](./non_exhaustive_match.md) — `match` on a sealed union misses variants without a wildcard.
- [`tyc::not_a_context_manager`](./not_a_context_manager.md) — `with` over a local class that lacks the context-manager protocol.
- [`tyc::not_callable`](./not_callable.md) — call on a non-callable value.
- [`tyc::nullable_use`](./nullable_use.md) — possibly-`None` value used where a non-`None` value was expected.
- [`tyc::operator_type_mismatch`](./operator_type_mismatch.md) — binary operator with incompatible operand types.
- [`tyc::orphan_py_import`](./orphan_py_import.md) — warning: relative `.py` import resolves outside `src/`.
- [`tyc::parallel_opportunity`](./parallel_opportunity.md) — advice (free-threaded targets): a comprehension or reduction is parallelisable but the enabling knob is off.
- [`tyc::parse`](./parse.md) — source file failed to parse.
- [`tyc::pattern_shadows_outer`](./pattern_shadows_outer.md) — `case` pattern captures a name that already exists as an immutable `let` in an enclosing scope.
- [`tyc::perf_keys_membership`](./perf_keys_membership.md) — advice: `x in d.keys()` — membership on the dict itself avoids the keys-view detour.
- [`tyc::perf_list_shift_in_loop`](./perf_list_shift_in_loop.md) — advice: `list.insert(0, …)` / `list.pop(0)` in a loop is O(n) per call — use `collections.deque`.
- [`tyc::perf_membership_in_loop`](./perf_membership_in_loop.md) — advice: repeated `x in LIST` inside a loop re-scans the list — hoist a `set`.
- [`tyc::perf_sort_in_loop`](./perf_sort_in_loop.md) — advice: sorting inside a loop re-sorts every iteration — sort once outside.
- [`tyc::perf_sorted_first`](./perf_sorted_first.md) — advice: `sorted(xs)[0]` / `[-1]` — `min()` / `max()` avoid the O(n log n) sort.
- [`tyc::perf_str_concat_in_loop`](./perf_str_concat_in_loop.md) — advice: growing a `str` with `+=` in a loop is quadratic — collect parts and `"".join(...)`.
- [`tyc::pub_name_collision`](./pub_name_collision.md) — `pub *` aggregation in `__init__.ty` finds two siblings exporting the same name.
- [`tyc::pub_star_outside_init`](./pub_star_outside_init.md) — advice: `pub *` outside `__init__.ty` is a no-op and the marker should be removed.
- [`tyc::python_semantic_drift`](./python_semantic_drift.md) — warning: Typhon rejects an expression CPython accepts.
- [`tyc::raise_non_exception`](./raise_non_exception.md) — `raise` operand is provably not a `BaseException` — would crash with `TypeError` at runtime.
- [`tyc::resource_not_managed`](./resource_not_managed.md) — bare assignment of a context-manager-returning call (`open`, `socket.socket`, `sqlite3.connect`, `tempfile.*`) not wrapped in `with`.
- [`tyc::result_error_mismatch`](./result_error_mismatch.md) — `?` forwards an `Err` whose type doesn't match the enclosing `Result`.
- [`tyc::return_in_except_star`](./return_in_except_star.md) — `return` / `break` / `continue` inside an `except*` handler — CPython rejects it at compile time, so the emitted Python would not import.
- [`tyc::self_outside_impl`](./self_outside_impl.md) — `self` referenced outside an `impl` method body.
- [`tyc::shared_mut_across_tasks`](./shared_mut_across_tasks.md) — advice (free-threaded targets): a `go`-spawned function writes shared mutable module state — a data race under real concurrency.
- [`tyc::stdlib_module_shadow`](./stdlib_module_shadow.md) — warning: project `.ty` file's stem matches a Python 3.13 stdlib top-level module name (`types`, `json`, `io`, …) and would intercept stdlib imports on `sys.path`.
- [`tyc::stub_mismatch`](./stub_mismatch.md) — `.dty` stub disagrees with the implementation module.
- [`tyc::tuple_index_out_of_range`](./tuple_index_out_of_range.md) — constant index out of range for a fixed-arity tuple.
- [`tyc::type_mismatch`](./type_mismatch.md) — value of one type used where another was expected.
- [`tyc::typevar_bound`](./typevar_bound.md) — inferred type argument doesn't satisfy the TypeVar's bound.
- [`tyc::typevar_import_rejected`](./typevar_import_rejected.md) — `from typing import TypeVar` rejected; use PEP 695 syntax.
- [`tyc::typing_alias_deprecated`](./typing_alias_deprecated.md) — deprecated capitalised alias from `typing` (`List`, `Dict`, …).
- [`tyc::typing_alias_in_annotation`](./typing_alias_in_annotation.md) — warning: deprecated capitalised `typing` alias (`List`, `Dict`, …) in an annotation — use the lowercase built-in.
- [`tyc::unknown_kwarg`](./unknown_kwarg.md) — keyword argument doesn't match any parameter name.
- [`tyc::unknown_module`](./unknown_module.md) — import names a module not in stdlib, project, or dependencies.
- [`tyc::unknown_name`](./unknown_name.md) — name used but never declared in any enclosing scope.
- [`tyc::unsafe_value_leak`](./unsafe_value_leak.md) — value introduced inside an `unsafe:` block returned from a function whose annotated return is concrete, without re-asserting the type at the boundary.
- [`tyc::unused_import`](./unused_import.md) — imported name is never used in the module.
- [`tyc::use_of_uninitialised`](./use_of_uninitialised.md) — read on a declare-only `let NAME: T` binding via a control-flow path that didn't assign it.
- [`tyc::go_outside_async`](./go_outside_async.md) — `go f(x)` outside an `async def`: no event loop is running there, so the spawn raised `RuntimeError` at the call.
- [`tyc::possibly_unbound`](./possibly_unbound.md) — warning: an ordinary function-local name is read on a path that may not (or certainly does not) assign it — a missing `else`, a read after `del`, an `except ... as` name used after its handler, a loop target after a possibly-empty loop.

Language-level reference pages (no `tyc::` code):

- [`freeze`](./freeze.md) — `freeze let X = expr` deep-immutable bindings.
- [`pub`](./pub.md) — module-level public-API modifier (`pub def`, `pub class`, `pub let`).
