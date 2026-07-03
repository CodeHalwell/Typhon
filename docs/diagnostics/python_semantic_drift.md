# tyc::python_semantic_drift

Warns when Typhon's type checker rejects an expression that CPython would
happily evaluate. Used as a regression signal during the Phase 5 Python-
semantic-alignment audit: Typhon aims to be a stricter superset of Python, so
any case where Typhon rejects valid CPython is by definition a bug in the
type checker, not the user's code.

## Example

```ty
let x: int = 1 + True  # warning: CPython accepts this (bool ⊆ int)
```

## Why

Typhon's design contract is that any well-typed CPython program is also a
well-typed Typhon program (modulo the binding-kind / annotation rules).
Whenever the checker over-rejects, the surrounding rule needs to be relaxed.
Surfacing the drift as a warning keeps builds green while the issue is
tracked.

## Fix

There's nothing to fix in user code — the warning is an instruction to *us*.
Open an issue with the offending snippet so the type-checker rule can be
adjusted to match CPython's semantics.

## Known drift cases

Cases closed:

- `bool ⊆ int` in arithmetic and assignment. `assignable` accepts
  `(Int, Bool)` and `(Float, Bool)` so `let x: int = True` /
  `let x: float = False` type-check; the BinOp arm folds
  `Int | Bool` operands into Int so `let x: int = 1 + True` works;
  unary `-True` / `~True` yield Int. The widening is one-way — `int`
  does NOT flow into `bool`. Regression tests
  `bool_assignable_to_int`, `bool_assignable_to_float`,
  `bool_arithmetic_with_int_yields_int`,
  `bool_arithmetic_both_bools_yields_int`,
  `bool_unary_arithmetic_yields_int`, `int_not_assignable_to_bool`
  in `tyc-types`.
- BinOp with a foreign-class operand no longer over-promotes to
  `float` (`4.0 * tensor + ...` used to reject because the
  `(Float, _)` arm caught `(Float, Class("Tensor"))` and returned
  `Float`; now only matches when both sides are numeric primitives
  and falls through to `Unknown` otherwise, letting the surrounding
  annotation drive). Found by the new corpus CI sweep against
  `examples/33-pytorch-tensors/pytorch_tensors.ty`. Test:
  `binop_with_class_operand_stays_unknown_not_float`.
- `or` / `and` truthy-union typing — `let chunk: str = update.text or ""`
  now flows through. See `docs/language.md` Python-semantics section.
- Generator function → `Iterable[T]` / `Iterator[T]` / async variants —
  structural conformance recognises the synthesised iterator shape.

More closures from the `stress/round-2026-05-23-drift/` probe sweep:

- **`tuple[int, int]` → `tuple[float, float]`** (`probe_tuple_widen`).
  Fixed. The `("tuple", 0) => Covariant` arm only relaxed slot 0; every
  position 1+ fell back to invariant via the default. Promoting the
  `tuple` / `Tuple` heads to fully covariant (immutable, so per-slot
  covariance is sound) closes this and any wider-arity sibling. Tests:
  `fixed_arity_tuple_widens_every_position` /
  `fixed_arity_tuple_rejects_unsound_widening` in `tyc-types`.

Cases verified-not-drift (CPython accepts at runtime, but static
rejection is sound and matches every other Python checker — mypy,
pyright, ty all reject these):

- **`list[int]` → `list[float]` parameter** (`probe_list_widen`).
  Mutable-container invariance. A `list[float]`-typed view can be
  written through, and writing a `float` would corrupt readers who hold
  the `list[int]` reference. Same rule applies to `set[T]` and the
  invariant `Dict[K, V]` positions.
- **`dict[int, str]` → `dict[float, str]`** (`probe_dict_widen`). Same
  cause on the key position. `Mapping[K, V]` is invariant in K and
  covariant in V (`dict.__setitem__` would otherwise hash through the
  wider type and corrupt the table).

The other thirteen probes pass cleanly — bool / int arithmetic,
int / float mixing, fixed-arity tuple covariance (now), empty container
literal inference, None-narrowing in ternaries, `str * int` repetition,
list concat, `enumerate` / `zip` / `dict.items` iteration, list/dict
comprehensions, ternary type unification, tuple unpacking with mixed
types.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/python_semantic_drift.md
