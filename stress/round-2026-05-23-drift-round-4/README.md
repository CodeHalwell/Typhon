# Stress round 2026-05-23 — Python semantic-drift audit (round 4)

A continuation of the Phase-5.2 Python-semantic alignment audit called out in
`docs/roadmap.md` step 5. This round focuses on edge-case Python idioms
introduced in the issue's candidate list:

- Walrus operator in comprehensions and conditionals
- Augmented assignment narrowing
- `yield from` generator delegation
- Match patterns with `*` capture
- String multiplication/concatenation type preservation
- `raise X from Y` clause typing
- Dict/list unpacking in function calls (`*args`, `**kwargs`)

## How to re-run

```bash
tyc check stress/round-2026-05-23-drift-round-4/probes.ty
```

Or to verify the probes run in Python:

```bash
tyc init /tmp/drift_probe
cp stress/round-2026-05-23-drift-round-4/probes.ty /tmp/drift_probe/src/main.ty
cd /tmp/drift_probe && tyc build
python3 -c "import sys; sys.path.insert(0, 'build'); from main import test_all; test_all(); print('Success!')"
```

## Result

**17 of 17 probes pass** — all candidate idioms from the issue are accepted by `tyc check`.

| # | Probe | Surface | Outcome |
|---|---|---|---|
| 1 | `probe_walrus_comprehension_basic` | `if (n := expr)` in comprehension | ✅ pass |
| 2 | `probe_walrus_comprehension_nested` | walrus in comprehension with tuple result | ✅ pass |
| 3 | `probe_walrus_conditional` | `if (count := len(x)) > 0` | ✅ pass |
| 4 | `probe_augmented_or_narrowing` | `let result: str = value or "default"` | ✅ pass |
| 5 | `probe_augmented_add_narrowing` | `x += 10` type preservation | ✅ pass |
| 6 | `probe_augmented_string_add` | `s += " world"` type preservation | ✅ pass |
| 7 | `probe_yield_from_simple` | `yield from list` | ✅ pass |
| 8 | `probe_yield_from_generator` | `yield from generator()` | ✅ pass |
| 9 | `probe_match_star_list` | `case [first, *rest]:` | ✅ pass |
| 10 | `probe_match_star_middle` | `case [first, *middle, last]:` | ✅ pass |
| 11 | `probe_string_multiplication` | `"ab" * 3 → str` | ✅ pass |
| 12 | `probe_string_concat` | `a + b + c → str` | ✅ pass |
| 13 | `probe_raise_from` | `raise X from Y` | ✅ pass |
| 14 | `probe_dict_unpack_call` | `f(**kwargs)` | ✅ pass |
| 15 | `probe_list_unpack_call` | `f(*args)` | ✅ pass |
| 16 | `probe_mixed_unpack_call` | `f(*args, **kwargs)` | ✅ pass |
| 17 | `probe_walrus_while` | walrus in while condition | ✅ pass |

## Findings

No over-rejections detected in this round. All tested idioms are already correctly accepted by the Typhon type checker.

### Notes on specific patterns

- **Walrus operator** (`if (n := expr)`): Fully supported in comprehensions, conditionals, and while loops
- **Augmented assignment**: Type preservation works correctly for `+=`, including narrowing `str? → str` via `or`
- **`yield from`**: Requires proper `Generator[T, None, None]` annotation; works with both iterables and generators
- **Match star-patterns**: `case [first, *rest]:` and `case [first, *middle, last]:` both parse and type-check correctly
- **String operations**: Multiplication and concatenation preserve `str` type as expected
- **`raise from`**: Fully supported with no type issues
- **Unpacking**: `*args` and `**kwargs` in function calls work correctly when types align

## Conclusion

This round demonstrates strong Python compatibility in Typhon's type system. All 17 probes representing common Python idioms pass without requiring `unsafe:` blocks or workarounds.

The broader Phase-5.2 audit goal (testing real third-party PyPI projects) remains future work — see roadmap step 1 and the PyPI sweep sub-item in the epic.
