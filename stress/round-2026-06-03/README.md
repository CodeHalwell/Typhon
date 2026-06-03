# Stress-test corpus — 2026-06-03 fresh round (v0.10.0)

Adversarial sweep against `tyc 0.10.0` (built from source). Python 3.11
was the only interpreter available, so the **in-process VM (`tyc run`)
was the primary runner** and most findings are VM-vs-CPython parity
breaks — directly relevant to v0.10.0's "VM as daily driver" pitch.

The language frontend (parser + type checker) held up extremely well:
every negative test was caught, and the *build* path (`tyc build` →
CPython) was correct on everything tried, including async `gather:`
lowering. The findings below are concentrated in the VM and in a few
checker coherence gaps.

`repros/` holds minimal `.ty` reproducers. Run each with:

```bash
TYC_SKIP_CHECK=1 tyc run repros/<file>.ty     # to reach the VM bug past a checker gate
tyc check repros/<file>.ty                     # to see the checker verdict
```

---

## Proposed feature — `enum` as a first-class keyword

`enum` should be a Typhon keyword that sugars over `enum.Enum`, exactly
as `model` sugars over `pydantic.BaseModel` and `class!` sugars over a
framework base. This keeps the "stricter superset" ergonomics and means
users never hand-write the `from enum import Enum` import or the base
list.

Proposed surface:

```python
enum Shape:
    CIRCLE
    SQUARE
    TRIANGLE

enum Color:
    RED = 1
    GREEN = 2
    BLUE = 4
```

Emitted Python:

```python
import enum

class Shape(enum.Enum):
    CIRCLE = enum.auto()
    SQUARE = enum.auto()
    TRIANGLE = enum.auto()

class Color(enum.Enum):
    RED = 1
    GREEN = 2
    BLUE = 4
```

Design notes / open questions:

- Bare members (no `= value`) auto-assign via `enum.auto()`.
- A modifier could select the base: `enum IntColor(int):` → `IntEnum`,
  `enum Perm(flag):` → `Flag` / `IntFlag`, `enum Status(str):` →
  `StrEnum`. Or keep it minimal and require `class! X(IntEnum):` for the
  exotic bases.
- Methods go in an `impl Shape:` block like every other Typhon type.
- Exhaustive `match` over an `enum` would be a natural follow-on
  (treat the member set as a closed set, like a sealed union), giving
  `tyc::non_exhaustive_match` coverage for enums.
- The VM **must** support this without the stack-overflow crash that
  raw `class X(enum.Enum):` currently triggers (finding C1 below).

---

## Findings

Severity: **C**ritical / **H**igh / **M**edium / **L**ow.
Surface: VM = `tyc run`; CHK = type checker; CLI = command surface.

### C1 — `enum.Enum` aborts the VM with a stack overflow  (VM)

`repros/enum.ty`. Member access alone (`Color.RED`) recurses infinitely
and aborts the **host Rust process**:

```
thread 'main' has overflowed its stack
fatal runtime error: stack overflow, aborting
```

Works perfectly via `tyc build` + CPython, so it is VM-only. A panic is
never acceptable here — the VM should support `Enum` (see the keyword
proposal) or reject it with a clean diagnostic.

### H1 — Dataclass `__eq__` not synthesized in the VM  (VM)

`repros/eq.ty`. `class` is Typhon's default form.

| | `a == b` (equal fields) | `print(a)` |
|---|---|---|
| `tyc build` + CPython | `True` | `P(x=1, y=2)` |
| `tyc run` (VM) | `False` | `<P instance>` |

Identity equality silently breaks every `assert result == Expected(...)`
test under the default runner.

### H2 — Dataclass `__repr__` not synthesized in the VM  (VM)

Same repro. `print(instance)` → `<P instance>` instead of
`P(x=1, y=2)`. Print-parity break.

### H3 — Frozen dataclasses are unhashable in the VM  (VM)

`repros/fkey.ty`. `frozen=True` makes a dataclass hashable in CPython,
but the VM raises `TypeError: unhashable type: 'instance'` when one is
used as a dict/set key. Same root cause as H1/H2 (no synthesized
dunders, no `__hash__`).

### H4 — Set / frozenset iteration order non-deterministic & non-CPython  (VM)

`repros/setdet.ty`. Three runs of `print(set(range(8)))` produced three
different orders; none matched CPython's `{0,1,…,7}`. Dict ordering was
fixed with `IndexMap` in v0.8.0; sets/frozensets need the same
`IndexSet` treatment. Today any program that prints/iterates a set is
non-reproducible and never matches build output.

### H5 — Checker doesn't validate attributes/methods on built-in types  (CHK + VM)

`repros/h03.ty`, `repros/h06.ty`, `repros/vmattr.ty`.
`attribute_not_found` fires for `User().bye()` but `int`/`str`/`list`/
`dict` get a free pass:

```python
let n: int = 5; print(n.foo)     # checker: clean → runtime AttributeError
"hi".frobnicate()                 # checker: clean → runtime AttributeError
```

Worse, in the VM `n.foo` does **not** raise — it returns a bogus
`<built-in function method>` object (H5b), masking the error entirely.

### M1 — `str % args` printf formatting unsupported  (CHK + VM)

`repros/fp01.ty`. `"%.2f" % (3.14,)` is rejected by the checker as
`tyc::operator_type_mismatch`, and the VM also raises `TypeError`.
The `%` operator must be special-cased when the LHS is `str`.

### M2 — `round()` is round-half-up, not banker's rounding  (VM)

`repros/c01.ty`.

| | `round(0.5)` | `round(2.5)` | `round(4.5)` | `round(2.675, 2)` |
|---|---|---|---|---|
| CPython | 0 | 2 | 4 | 2.67 |
| VM | 1 | 3 | 5 | 2.68 |

### M3 — f-string `%` percentage format type broken  (VM)

`repros/pct.ty`. `f"{0.5:.0%}"` → VM `0` (should be `50%`);
`f"{0.1234:.1%}"` → `0.1` (should be `12.3%`). It ignores `%`: no ×100,
no `%` suffix.

### M4 — Large-float repr never uses scientific notation  (VM)

`repr(1e20)` → VM `100000000000000000000.0`, CPython `1e+20`.

### M5 — `defaultdict(factory)` ignores its factory  (VM)

`repros/coll.ty`. `groups["missing"].append(x)` raises `KeyError` in the
VM; it behaves like a plain dict.

### M6 — Checker holes on built-in subscript / iter / key types  (CHK)

`repros/h01.ty`, `repros/h04.ty`. All clean at check time, crash at
runtime:
- `int_val[0]` — subscripting a non-subscriptable type
- `for x in int_val` — iterating a non-iterable
- `d[5]` where `d: dict[str, int]` — wrong subscript key type

### L1 — `tyc build file.ty` rejects single files  (CLI)

`tyc build foo.ty` treats the arg as a project dir
(`source directory '.../foo.ty/src' does not exist`), yet
`tyc run foo.ty` accepts single files. Inconsistent.

### L2 — Async is unrunnable in the default runner  (VM + CLI)

The VM has no `asyncio` (`ImportError: tyc-vm cannot import 'asyncio'`),
and `tyc run --compile` requires a project layout *and* Python 3.13+. So
the flagship `gather:` / `go` / `await` surface can only be exercised
via a full `tyc build` of a scaffolded project. The emitted async code
is clean and correct (TaskGroup lowering verified on 3.11) — purely a
runner-ergonomics gap.

---

## What held up (regression anchors)

- Type checker caught **every** negative test: missing annotation,
  nullable misuse, non-exhaustive match, immutable/frozen assign,
  newtype violation, return/arg-type mismatch, interface signature
  conformance, Result error-type mismatch, `?`-in-non-Result, arg
  count, not-callable, operator mismatch.
- VM ran correctly: bigints (`2**100`), `Result`/`?`/`with`-chains/
  combinators, sealed unions + distributed `impl`, generics + `.map`,
  closures with `nonlocal`, `@memo`, walrus, every comprehension
  variant, `*args`/`**kwargs`/defaults, custom exceptions (`class!`)
  with `try/except/else/finally`, per-instance `default_factory`,
  `model`/pydantic field access, JSON, `Counter`, complex
  `sorted(key=...)`, pipes, guards.
- End-to-end programs correct first try: AST interpreter, gradient
  descent (converged to x=3.0000), 2×2 matmul, agent tool-dispatch
  loop, HTTP routing API with newtype IDs.

## Suggested fix priority

1. **C1** enum VM crash (and ship the `enum` keyword).
2. **H1/H2/H3** synthesize dataclass `__eq__`/`__repr__`/`__hash__`
   in the VM — biggest parity win.
3. **H4** `IndexSet` for deterministic, CPython-matching set order.
4. **H5/M6** extend attribute/subscript/iter/key validation to
   built-in types; fix the VM's bogus-attribute fallback to raise.
5. **M1** `str %` formatting.
6. **M2–M5** VM numeric/format faithfulness.
7. **L1/L2** CLI ergonomics.
