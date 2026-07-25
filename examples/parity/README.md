# VM ↔ CPython parity corpus

`docs/vm.md` states the rule this directory enforces:

> The VM is a parallel execution surface, not a stage in the build pipeline.
> It must stay a drop-in for `tyc build && python` — **VM/CPython divergences
> are bugs.**

Every program here is written to be *output-deterministic* and to lean on the
value semantics most likely to drift between a tree-walking interpreter and
CPython: integer and float edge cases, container and float `repr`, dunder
dispatch, generator materialisation, pattern matching, comprehension scoping,
and the lowering of Typhon-specific forms.

```bash
tyc run examples/parity/num_int_semantics.ty      # VM
# vs
tyc build && python build/main.py                 # compiled
```

## Two directories, two opposite assertions

| Path | Assertion | Enforced by |
|---|---|---|
| `examples/parity/*.ty` | stdout is **byte-identical** under both paths | `vm_output_matches_cpython` |
| `examples/parity/divergent/*.ty` | the two paths **still differ** | `documented_divergences_still_diverge` |

Both live in `tyc/crates/tyc/tests/parity_corpus.rs` and run under
`cargo test --workspace`. The second is a tripwire: these files are checked in
*because* they misbehave, so when one starts agreeing the test fails and the
file gets promoted out of `divergent/`.

The suite skips itself when no CPython ≥3.13 is on `PATH`, since the emitted
code uses 3.13+ syntax.

## What the matching corpus covers

| File | Exercises |
|---|---|
| `num_int_semantics.ty` | floor-div / modulo signs, `divmod`, big-int `**`, shifts, `~`, `round` half-to-even |
| `num_float_repr.ty` | shortest round-trip `repr`, `-0.0`, `inf`, float `%` / `//` signs, format specs |
| `collections_ops.ty` | slicing with steps and negatives, sort keys, dict/set algebra, `zip` / `enumerate` |
| `comprehension_edges.ty` | nested and multi-`for` comprehensions, dict/set comps, tuple-unpack targets, generator args |
| `class_protocols.ty` | arithmetic / reflected / comparison dunders, `__len__` / `__getitem__` / `__contains__` / `__hash__`, `@property`, `@classmethod`, `@staticmethod`, inheritance |
| `gen_iterators.ty` | `yield`, `yield from`, hand-written `__iter__` / `__next__`, `itertools` |
| `generics_variance.ty` | PEP 695 generic functions and classes, `impl[T]`, `Sequence` / `Iterable` widening, subclass covariance |
| `emit_precedence.ty` | operator precedence surviving lowering — pipes vs arithmetic, chained comparisons, `-2 ** 2`, ternaries, f-string specs |
| `recursion_and_depth.ty` | deep and mutual recursion, big-int factorials, linked-list walks |
| `decorators_misc.ty` | `@memo`, `@pure`, `functools.cache`, frozen-class equality and hashing, per-instance `default_factory` |
| `interfaces_extend.ty` | structural interface conformance, `impl` distributed over a sealed union, `extend str:` / `extend list:` |
| `vm_asbang_cast.ty` | `as!` in value, comprehension and union positions, and its runtime `TypeError` on a wrong shape |

## Adding a case

Write a program with deterministic output (no clocks, no unseeded randomness,
no dict-iteration-order assumptions beyond insertion order), drop it in, and
run `cargo test --workspace parity`. If the two paths disagree you have found a
bug: either fix it, or move the file into `divergent/` with a `# DIVERGENT:`
header recording both outputs and what the correct behaviour is.
