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

**13 of 15 probes pass** after the fixed-arity tuple covariance fix.
The two remaining rejections are intentional invariance for mutable
containers and match every other Python static checker.

| # | Probe | Surface | Outcome |
|---|---|---|---|
| 4 | `probe_tuple_widen` | `tuple[int, int] → tuple[float, float]` | **fixed** in `generic_param_variance` |
| 3 | `probe_list_widen` | `list[int] → list[float]` parameter | sound rejection (mutable-container invariance) |
| 5 | `probe_dict_widen` | `dict[int, str] → dict[float, str]` | sound rejection (`Mapping` is invariant in K) |
| 1, 2, 6–15 | rest | bool/int arith, int/float mixing, empty container literal inference, None-narrowing ternary, `str * int`, list concat, enumerate/zip/dict.items iteration, comprehensions, ternary unification, tuple unpacking | pass |

## Fix landed

The `("tuple", 0) => Covariant` arm in `generic_param_variance` only
covered slot 0, so a fixed-arity `tuple[int, int]` tripped invariance
at slot 1+ and rejected. Promoted `tuple` / `Tuple` to a head-level
early return so every slot is covariant uniformly — sound because
tuples are immutable. See `tyc-types/src/lib.rs:759` and the two new
regression tests `fixed_arity_tuple_widens_every_position` and
`fixed_arity_tuple_rejects_unsound_widening`.

## Not drift (after analysis)

`list[int] → list[float]` and `dict[int, str] → dict[float, str]` are
correctly rejected: writing through a `list[float]` view would let a
later read of the `list[int]` reference observe a `float`. Mypy,
pyright, and ty all reject the same patterns. CPython accepts them
only because the runtime is duck-typed and `float.__radd__(int)` covers
arithmetic — the static rule guards against the mutation hazard the
runtime can't see.

The larger Phase-5.2 audit (longer corpus, more idioms) is still
open work — see roadmap step 5.
