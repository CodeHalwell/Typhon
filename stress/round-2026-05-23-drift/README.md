# Stress round 2026-05-23 — Python semantic-drift audit (mini)

A small audit run as part of the Phase-5.2 broadening called out in
`docs/roadmap.md`. Fifteen probes, each one an idiom CPython 3.13
accepts at runtime; `tyc check` flags any that the type checker
over-rejects.

## How to re-run

```bash
tyc init /tmp/drift_probe
cp 15-probes.ty /tmp/drift_probe/src/main.ty
cd /tmp/drift_probe && tyc check
```

## Result

12 of 15 probes pass. Three drift cases remain, all the same root cause:
**container element variance under `int → float` widening is not
recognised**. CPython accepts every site because `float.__radd__(int)`
(and friends) cover the runtime arithmetic.

| # | Probe | Surface | Outcome |
|---|---|---|---|
| 3 | `probe_list_widen` | `list[int] → list[float]` parameter | `tyc::type_mismatch` (drift) |
| 4 | `probe_tuple_widen` | `tuple[int, int] → tuple[float, float]` | `tyc::type_mismatch` (drift) |
| 5 | `probe_dict_widen` | `dict[int, str] → dict[float, str]` | `tyc::type_mismatch` (drift) |
| 1, 2, 6–15 | rest | bool/int arith, int/float mixing, empty container literal inference, None-narrowing ternary, `str * int`, list concat, enumerate/zip/dict.items iteration, comprehensions, ternary unification, tuple unpacking | pass |

## Suggested fix

The `bool ⊆ int` widening machinery added in `2225099` is the right
template. Extending it to act on the element type when the container
head unifies (and on each fixed-arity tuple slot) closes all three
cases at once. Track this as a follow-up to the Phase-5.2 audit;
catalogued in `docs/diagnostics/python_semantic_drift.md`.

Notably, the larger Phase-5.2 audit (longer corpus, more idioms) is
still open work — see roadmap step 5.
