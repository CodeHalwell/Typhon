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

See https://typhon.dev/lang/diagnostics/python_semantic_drift
