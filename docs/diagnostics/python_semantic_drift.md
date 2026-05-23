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

- `bool ⊆ int` in arithmetic and assignment — landed in `2225099`, surfaced
  by this diagnostic in `d46a1aa`. `let x: int = 1 + True` type-checks.
- `or` / `and` truthy-union typing — `let chunk: str = update.text or ""`
  now flows through. See `docs/language.md` Python-semantics section.
- Generator function → `Iterable[T]` / `Iterator[T]` / async variants —
  structural conformance recognises the synthesised iterator shape.

Cases still open (filed 2026-05-23 from the
`stress/round-2026-05-23-drift/` probe sweep):

- **`list[int]` → `list[float]` parameter** (`probe_list_widen`). Container
  element variance under int → float widening is not recognised; CPython
  accepts every site because `float.__radd__(int)` etc. cover the runtime
  arithmetic. The fix is the same widening machinery the `bool ⊆ int`
  pass uses, extended to act on the element type when the container head
  unifies.
- **`tuple[int, int]` → `tuple[float, float]`** (`probe_tuple_widen`).
  Same root cause; the existing `tuple` variadic variance handling
  (`O3`) covers the unbounded shape but not the per-element widening for
  fixed-arity tuples.
- **`dict[int, str]` → `dict[float, str]`** (`probe_dict_widen`). Same root
  cause on the key position.

The other twelve probes pass cleanly — bool / int arithmetic, int / float
mixing, empty container literal inference, None-narrowing in ternaries,
`str * int` repetition, list concat, `enumerate` / `zip` / `dict.items`
iteration, list/dict comprehensions, ternary type unification, tuple
unpacking with mixed types.

See https://typhon.dev/lang/diagnostics/python_semantic_drift
