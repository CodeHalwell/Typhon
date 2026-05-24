# Type System Frontier - Implementation Summary

This document summarizes the implementation of three independent type system enhancements
from [Epic: type-system frontier — HKT, full variance inference, comptime types-as-values](https://github.com/CodeHalwell/Typhon/pull/113).

## 1. Higher-Kinded Types (HKT) — Full Unification ✅

### What was implemented

**Foundation (v0.5.0):**
- **New `Type::TypeConstructor` variant**: Represents type constructors with unbound parameters
- **`F[_]` syntax recognition**: Parser recognizes higher-kinded type parameters (only when F is a declared type parameter)
- **Display support**: `Functor[_]`, `Bifunctor[_, _]` display correctly

**Full unification (v0.5.1):**
- **`bind_typevars_inner`**: New function replacing `bind_typevars` that accepts an `hkt_heads: &[String]` parameter. When `fh` is in the HKT heads set and the actual head differs, it binds `fh → Class(actual_head)` and recurses on the type arguments.
- **`collect_single_uppercase_generic_heads`**: Scans formal types for `Generic("F", ...)` occurrences where `F` is a single uppercase letter (the universal TypeVar/HKT naming convention). Complements `walk_typevars` which catches explicit `TypeConstructor` forms.
- **`substitute_typevars`**: Now substitutes Generic heads — when processing `Generic(h, args)`, it looks up `h` in the bindings map and, if bound as `Class(new_head)`, produces `Generic(new_head, substituted_args)`. This enables `F[B]` → `list[int]` once `F → list` is in the binding map.
- **`compute_bidirectional_bindings`**: Pre-computes `hkt_heads` by running both `walk_typevars` (for explicit `TypeConstructor` forms) and `collect_single_uppercase_generic_heads` (for `F[A]` method-sig forms) over all formal params and the return type; threads this set into all `bind_typevars_inner` calls.

### Code locations

- Type enum: `tyc/crates/tyc-types/src/lib.rs` (search `TypeConstructor`)
- HKT head collection: `collect_single_uppercase_generic_heads` in `tyc-types`
- Inner binding with HKT: `bind_typevars_inner` in `tyc-types`
- Substitute with head rewrite: `substitute_typevars` in `tyc-types`
- Bidirectional entry point: `compute_bidirectional_bindings` in `tyc-types`

### Example usage

```python
# Now fully functional — HKT unification works end-to-end
class Functor[F[_]]:
    pass

impl[F[_]] Functor[F[_]]:
    def fmap[A, B](fa: F[A], f: Callable[[A], B]) -> F[B]:
        ...

# At a call site, F is inferred from the concrete argument:
# fmap(some_list_int, str) → F=list, A=int, B=str → returns list[str]
```

### Tests

Nine tests covering HKT:
- `hkt_type_constructor_single_underscore`
- `hkt_type_constructor_multiple_underscores`
- `hkt_type_constructor_display`
- `hkt_type_constructor_is_assignable`
- `hkt_type_constructor_binding`
- `hkt_bind_single_uppercase_head_generic_method_sig`
- `hkt_substitute_typevars_replaces_constructor_head`
- `hkt_bind_and_substitute_end_to_end`
- `hkt_bind_two_arg_constructor`
- `hkt_explicit_type_constructor_binds_head`

### Remaining open items

- Multi-level HKT (type constructors applied to other type constructors)
- Kind inference for higher-arity constructors beyond single-letter naming convention
- Integration with bounded type parameters (`F[_]: Functor`)

## 2. Comptime Types-as-Values ✅

### What was implemented

- **New `ComptimeValue::Type` variant**: Types can now be comptime values
- **Type name recognition**: Runtime-resolvable built-in types only: `int`, `str`, `bool`, `float`, `bytes`, `None`, `type`, `object`
- **`Any` excluded**: Not a runtime builtin; would cause NameError without importing from `typing`
- **Emission strategy**: Type values emit as type expressions, not string literals
- **Full integration**: Works with all existing comptime operations

### Code locations

- ComptimeValue enum: `tyc/crates/tyc-analyse/src/lib.rs:104`
- Type recognition: `tyc/crates/tyc-analyse/src/lib.rs:381-395`
- Display: `tyc/crates/tyc-analyse/src/lib.rs:162-166`
- Tests: `tyc/crates/tyc/tests/build_features.rs:1542-1621`

### Example usage

```python
# Define a type at compile time
comptime let T: type = int

# Use it in annotations
let x: T = 42  # Equivalent to: let x: int = 42

# Type names are first-class comptime values
comptime let types = [int, str, bool]
comptime let name = str(int)  # "int"
```

### Design notes

- Type values emit as bare type names, not string literals
- `comptime let T: type = int` emits `int` not `"int"`
- This allows types to be used in annotation positions
- The `type` annotation is recognized at comptime only

## 3. Full Variance Inference ✅

### What was implemented

**Built-in variance table (v0.5.0):**
- **Mutable containers** (invariant): `list[0]`, `dict[0]`, `dict[1]`, `set[0]`
- **Read-only views** (covariant): `Sequence[0]`, `Iterable[0]`, `Iterator[0]`, `tuple[*]`
- **Mapping types**: `Mapping[0]` invariant (keys), `Mapping[1]` covariant (values)
- **Callable**: `Callable[0]` contravariant (args), `Callable[1]` covariant (return)
- **Result**: Both `Result[0]` and `Result[1]` are covariant

**User-declared variance inference (v0.5.1):**
- **`type_appears_in`**: Recursive helper that checks whether a named type variable appears anywhere inside a `Type`.
- **`infer_class_variance_from_shape`**: After all `impl`/`extend` blocks are merged into the class shape, walks fields and method signatures to classify each type param's variance:
  - Field in **non-frozen** class → Invariant (both covariant read + contravariant write)
  - Field in **frozen** class → Covariant (read-only)
  - Method **parameter** type → Contravariant
  - Method **return** type → Covariant
  - Both positions → Invariant; neither position (phantom) → Invariant (conservative)
- **`Checker::user_class_variance`**: New `HashMap<String, Vec<Variance>>` field populated in a fourth pass at the end of `collect_classes_and_functions`.
- **`is_assignable`**: Consults `user_class_variance` before falling back to `generic_param_variance` for the built-in table.

### Code locations

- Variance enum: `tyc/crates/tyc-types/src/lib.rs` (search `enum Variance`)
- Built-in mapping: `generic_param_variance` in `tyc-types`
- `type_appears_in`: `tyc-types` (before `collect_classes_and_functions`)
- `infer_class_variance_from_shape`: `tyc-types` (before `collect_classes_and_functions`)
- Checker field: `user_class_variance` in `Checker` struct
- Fourth pass: end of `collect_classes_and_functions`
- Assignability dispatch: `is_assignable`, `Generic` same-head arm

### Example

```python
# Inferred Covariant: T only in field of frozen class + getter return
class Reader[T] frozen:
    value: T

impl[T] Reader[T]:
    def get(self) -> T: return self.value

let rs: Reader[str] = Reader(value="hello")
let ro: Reader[object] = rs    # ✅ Reader[str] is a Reader[object]

# Inferred Contravariant: T only in method parameter
class Sink[T]:
    pass

impl[T] Sink[T]:
    def push(self, value: T) -> None: print(value)

let so: Sink[object] = Sink()
let ss: Sink[str] = so    # ✅ Sink[object] is a Sink[str]

# Invariant: non-frozen field (default) — no implicit subtyping
class Box[T]:
    value: T    # read-write → Invariant
```

### Tests

Seven new variance inference tests:
- `variance_read_only_field_and_getter_is_covariant`
- `variance_mutable_field_is_invariant`
- `variance_write_only_method_is_contravariant`
- `variance_param_and_return_is_invariant`
- `variance_multi_param_frozen_fields`
- `variance_phantom_type_param_is_invariant`
- `type_appears_in_nested_generic`

## Test Results

All tests pass (v0.5.1):
- **tyc-types**: 254 tests (including 10 HKT tests, 7 variance tests)
- **tyc-analyse**: 138 tests (ComptimeValue::Type integrated)
- **Total workspace**: all packages green

## References

- Epic PR: [type-system frontier — HKT, full variance inference, comptime types-as-values](https://github.com/CodeHalwell/Typhon/pull/113)
- Roadmap: `docs/roadmap.md` Concrete next step #2 and Phase 4+
- Implementation PR: [#113](https://github.com/CodeHalwell/Typhon/pull/113)
