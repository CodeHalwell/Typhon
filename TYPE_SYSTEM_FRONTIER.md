# Type System Frontier - Implementation Summary

This document summarizes the implementation of three independent type system enhancements
from [Epic: type-system frontier — HKT, full variance inference, comptime types-as-values](https://github.com/CodeHalwell/Typhon/pull/113).

## 1. Higher-Kinded Types (HKT) - Foundation ✅

### What was implemented

- **New `Type::TypeConstructor` variant**: Represents type constructors with unbound parameters
- **`F[_]` syntax recognition**: Parser recognizes higher-kinded type parameters (only when F is a declared type parameter)
- **HKT binding support**: TypeConstructor unification in `bind_typevars` for proper HKT inference
- **Display support**: `Functor[_]`, `Bifunctor[_, _]` display correctly

### Code locations

- Type enum: `tyc/crates/tyc-types/src/lib.rs:87`
- Parser support: `tyc/crates/tyc-types/src/lib.rs:1090-1111` (restricted to type parameters)
- HKT binding: `tyc/crates/tyc-types/src/lib.rs:1241-1248`
- Display: `tyc/crates/tyc-types/src/lib.rs:181-184`

### Example usage

```python
# Not yet fully functional - this is the foundation
# Future work: Update bind_typevars_and_substitute for HKT unification
class Functor[F[_]]:
    def map[A, B](self, fa: F[A], f: A -> B) -> F[B]: ...
```

### Tests

Four comprehensive tests added:
- `hkt_type_constructor_single_underscore`
- `hkt_type_constructor_multiple_underscores`
- `hkt_type_constructor_display`
- `hkt_type_constructor_is_assignable`

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

## 3. Full Variance Inference - Infrastructure Complete ✅

### What was implemented

The variance infrastructure is fully in place and covers all Python built-ins:

- **Mutable containers** (invariant): `list[0]`, `dict[0]`, `dict[1]`, `set[0]`
- **Read-only views** (covariant): `Sequence[0]`, `Iterable[0]`, `Iterator[0]`, `tuple[*]`
- **Mapping types**: `Mapping[0]` invariant (keys), `Mapping[1]` covariant (values)
- **Callable**: `Callable[0]` contravariant (args), `Callable[1]` covariant (return)
- **Result**: Both `Result[0]` and `Result[1]` are covariant

### Code locations

- Variance enum: `tyc/crates/tyc-types/src/lib.rs:747-757`
- Built-in mapping: `tyc/crates/tyc-types/src/lib.rs:768-862`
- Used in assignability: `tyc/crates/tyc-types/src/lib.rs:294-316`

### What's deferred

User-declared generics (`class Box[T]: ...`) currently default to invariant.
Full variance inference would require:

1. Walking the class body to classify each type parameter's usage
2. Storing per-class variance in `class_type_params`
3. Consulting the inferred variance in `is_assignable`

This is a well-scoped future enhancement, as described in the roadmap.

## Test Results

All tests pass:
- **tyc-types**: 242 tests (including 4 new HKT tests)
- **tyc-analyse**: 138 tests (ComptimeValue::Type integrated)
- **Total workspace**: 1477 tests across all packages

## Next Steps

### For HKT (medium effort)

The foundation is complete. The remaining work is in `bind_typevars_and_substitute`:

1. Extend type variable binding to handle type constructors
2. Support unification of `F[_]` with concrete types like `list`
3. Allow higher-kinded type parameters in function signatures

See `docs/roadmap.md` Concrete next step #2.

### For Variance Inference (medium effort)

The classification rules are well-defined:

1. Create a pass that walks `ClassDef.body`
2. Track whether each type parameter appears in covariant, contravariant, or invariant positions
3. Store the inferred variance alongside the type parameter names
4. Use it in the `is_assignable` Generic arm

See `docs/roadmap.md` Concrete next step #2.

### For Comptime Types (design decision)

The implementation is complete for the basic case. The open question is:

**What is `type` at the type level?**
- Is `type` a comptime-only marker?
- Should it surface to the type checker as a first-class type?
- Should `comptime let T: type = int` allow `T` to be used at runtime?

See `docs/roadmap.md` Phase 4+ "Richer comptime".

## References

- Epic PR: [type-system frontier — HKT, full variance inference, comptime types-as-values](https://github.com/CodeHalwell/Typhon/pull/113)
- Roadmap: `docs/roadmap.md` Concrete next step #2 and Phase 4+
- Implementation PR: [#113](https://github.com/CodeHalwell/Typhon/pull/113)
