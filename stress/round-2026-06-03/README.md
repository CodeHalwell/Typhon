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

**H4b — set equality is order-sensitive in the VM.** `repros/eq2.ty`.
`{1,2,3} == {3,2,1}` → VM `False`, CPython `True`. Set `==` ignores
set semantics (same underlying order-sensitive representation as H4).
list / dict / tuple / nested equality all compare correctly — the bug
is specific to sets and to dataclass instances (H1).

### H0 — `@dataclass(slots=True)` + zero-arg `super()` crashes at runtime  (BUILD — production output)

`repros/cls.ty`. The single most serious finding, because it is in the
**emitted production code**, not the VM. Typhon emits
`@dataclass(slots=True)` for every `class`, and `slots=True` rebuilds
the class object, orphaning the `__class__` closure cell that a bare
`super()` relies on:

```python
@dataclasses.dataclass(slots=True)
class Derived(Base):
    def describe(self) -> str:
        parent: str = super().describe()   # TypeError at runtime
        return ...
```

```
TypeError: super(type, obj): obj must be an instance or subtype of type
```

Any Typhon program combining the default `class` form + inheritance +
`super()` ships broken. **Fix:** emit the explicit two-argument form
`super(Derived, self)` whenever a method body uses `super()` (verified
workaround — two-arg `super` does not depend on the `__class__` cell).
`super()` is also broken in the VM (`AttributeError: module '<super>'
has no attribute 'describe'`), separately.

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

### M7 — `str.split(maxsplit=...)` ignores the maxsplit argument  (VM)

`repros/split2.ty`. `"a b c".split(" ", 1)` → VM `['a', 'b', 'c']`
(CPython `['a', 'b c']`). Both positional and keyword `maxsplit` are
dropped; `split()` / `split(sep)` without maxsplit are correct.

### M8 — Callable instances (`__call__`) unsupported in the VM  (VM)

`repros/dunder.ty`. An instance of a class with `def __call__(self, …)`
raises `TypeError: 'instance' object is not callable` in the VM. The
checker accepts it (it is valid Python). Common for functors /
decorator objects. `@property` does work in the VM.

### M9 — f-string `{x=}` debug specifier drops the `name=` prefix  (VM)

`repros/misc.ty`. `f"{val=}"` → VM `42`, CPython `val=42`. The
self-documenting-expression form (3.8+) is widely used in debugging.

### M10 — `bytes` methods (`decode`, `hex`, …) unsupported in the VM  (VM)

`repros/by.ty`. `b"hello".decode()` →
`AttributeError: 'bytes' object has no method 'decode'`, halting the
program. `repr` / `len` / indexing work, but the method surface is
mostly missing. Checker accepts (valid).

### M11 — `__post_init__` is never called in the VM  (VM)

`repros/pi.ty`. A dataclass with `def __post_init__(self)` that derives
a field has that hook honoured by `tyc build` + CPython (`area` →
12.57) but ignored by the VM (`area` stays 0.0). Silent wrong answer.

### M12 — Multi-level inheritance drops grandparent fields in the VM  (VM)

`repros/lvl.ty`. With `class A` → `class B(A)` → `class C(B)`,
constructing `C(x=…, y=…, z=…)` raises
`TypeError: C() got unexpected keyword argument(s): x` in the VM — the
grandparent field `x` is not accumulated. 2-level inheritance
(`B(x=…, y=…)`) works; only 3+ levels break. The v0.8.0 "subclass
constructors inherit fields" fix evidently only walks one MRO hop.

### Notes / non-bugs

- `@contextmanager` generator-based context managers raise a **graceful**
  `NotImplementedError` in the VM ("the VM evaluates generators
  eagerly") rather than crashing — but this contradicts the v0.9.0
  changelog claim that the `contextlib` shim works under `tyc run`.
  `repros/cm.ty`.
- Plain generators (`yield`, `while`+`yield`, `list(gen())`, `next()`,
  comprehensions over a generator) all work correctly. (An earlier
  "first value dropped" alarm was a `tail -n` truncation artifact, not
  a bug.)

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

## Strong points confirmed this round (regression anchors)

- Bounded TypeVars enforce their bound: `max_of[T: int]("a", "b")` →
  `tyc::typevar_bound` (docs undersell this as "partial").
- `while cur is not None:` narrows attribute chains across the loop;
  nested optional `guard` chains; guarded exhaustive `match`; generic
  inference into recursive generics (`Tree[T].size()`) — all clean and
  correct, and the recursive generic runs in the VM.
- All int/float format specs verified correct in the VM: binary/octal/
  hex, sign, fill+align, width, `.Nf`, `.Ne`. (The `%` percent type
  M3 and large-float repr M4 are the only format exceptions.)
- super() aside, `@property`, `@staticmethod`, `@classmethod` work in
  the VM.

## Suggested fix priority

1. **H0** emit two-arg `super(Cls, self)` — production output is broken
   today for the default class + inheritance + `super()` combo.
2. **C1** enum VM crash (and ship the `enum` keyword).
3. **H1/H2/H3** synthesize dataclass `__eq__`/`__repr__`/`__hash__`
   in the VM — biggest parity win.
4. **H4/H4b** `IndexSet` for deterministic, CPython-matching set order
   and set equality.
5. **H5/M6** extend attribute/subscript/iter/key validation to
   built-in types; fix the VM's bogus-attribute fallback to raise.
6. **M1** `str %` formatting; **M7** `split(maxsplit=)`; **M8**
   `__call__`; **M9** f-string `{x=}`; **M10** `bytes` methods.
7. **M2–M5** VM numeric/format faithfulness (round, percent, float
   repr, defaultdict).
8. **L1/L2** CLI ergonomics.
