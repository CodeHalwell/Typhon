# Stress-test corpus — 2026-06-21 (v0.15.6)

**Goal:** confirm that a broad cross-section of "things you actually write in
Python" expressed as Typhon compiles to **valid, runnable Python**. Python can
express almost anything, so the corpus deliberately spans many independent
language surfaces and lowering paths rather than going deep on one.

## Methodology

Each of the **130** `.ty` programs in `repros/` is run through three paths by
`harness.sh` and compared:

1. **`tyc check`** — frontend (parser + resolver + type checker).
2. **`tyc build` + `python3.13`** — the **production path**. This is what
   "compiles to valid, runnable Python" actually means, and is the primary
   verdict.
3. **`tyc run`** — the in-process tree-walking VM (secondary; the VM
   intentionally does not cover every surface).

A program is `PASS` when `tyc build` emits Python that `python3.13` runs to a
clean exit. `{VM-DIVERGE}` flags a program whose production path passed but
whose VM output/exit differed (a VM-only issue, never a codegen issue).

```bash
bash harness.sh repros/*.ty        # TYC=… PYTHON=… overridable
```

Requires `pydantic` installed for the `model` repro (`36`); everything else is
stdlib-only.

## Result — 130 / 130 compile to valid, runnable Python

```
total=130 pass=130 buildfail=0 runfail=0 checkfail=0 vm_diverge=37
```

Every program type-checks, builds, and runs correctly on the compiled
(`tyc build` → CPython 3.13) path. **Zero codegen defects** were found across
the whole 130-program corpus.

### Three real type-checker build-blockers found and fixed

All three were **false positives** that blocked the build on code that runs
correctly under CPython — exactly what this round exists to surface.

- **`__call__` → `Callable` (repro `95`).** An instance of a class with a
  `__call__` method (a "functor" / callable object) was rejected where a
  `Callable[[int], int]` parameter was expected — `tyc::type_mismatch`
  (`expected (int) -> int, found Multiplier`). **Fixed** in `tyc-types`:
  `is_assignable` now recognises a `__call__`-bearing class as structurally
  conforming to a function type (look up `__call__`, rebuild its signature,
  re-run the function-to-function variance check). A class *without* `__call__`
  is still rejected, so the change can only relax assignability.
- **`plain class` custom `__init__` (repro `106`).** A `plain class` with a
  hand-written `__init__(self, data: ...)` assigning to a `_data` field, built
  as `DynamicRecord(data=...)`, false-fired `tyc::unknown_kwarg` ("did you mean
  `_data`?"). The constructor *arity* check already exempted `plain class` /
  `class!`; the adjacent *unknown-kwarg* loop now does too. A normal `class`
  still reports a misspelled kwarg.
- **`Counter + Counter` / `Counter - Counter` (repro `117`).** Multiset
  add/subtract on `collections.Counter` was rejected with
  `tyc::operator_type_mismatch`, even though Counter overloads both (and the
  set-style `&`/`|` already passed). `operator_operands_compatible` now accepts
  Counter on the `+` and `-` arms. `Counter + int` still errors.

Regression tests added in `tyc-types` (`callable_instance_conforms_to_callable_param`,
`non_callable_instance_rejected_as_callable_param`,
`normal_class_unknown_kwarg_still_rejected`, `counter_add_and_sub_accepted`,
`counter_plus_int_still_rejected`; the plain-class half is verified end-to-end
against the binary, since the unit harness doesn't populate `plain_classes`).
Full workspace test suite green.

### The type checker caught two deliberately-buggy probes at compile time

Two programs were written to demonstrate a *runtime* error and were instead
rejected at **check time** — Typhon's static guarantees are stronger than the
runtime behaviour they were probing:

- `17` — a literal `10 // 0` was caught by `tyc::div_by_zero_literal` (rewrote
  the divisor through a runtime value to reach the `ZeroDivisionError` handler).
- `89` — a frozen-field write `base.port = 9999` was caught by
  `tyc::frozen_assign`, stronger than CPython's runtime `FrozenInstanceError`
  (rewrote it through `setattr` to reach the runtime guard).

## Coverage (90 programs)

| # | Area | # | Area |
|---|---|---|---|
| 01 | arithmetic, bigint, complex, bases | 46 | mutual recursion + binary tree |
| 02 | string methods + f-string formatting | 47 | `global` / module-level `mut` state |
| 03 | list/dict/set comprehensions, nested | 48 | `as!` checked boundary cast |
| 04 | `collections` (deque/Counter/defaultdict/OrderedDict) | 49 | iterator protocol (`__iter__`/`__next__`) |
| 05 | `itertools` | 50 | container protocol (`__getitem__`/`__setitem__`/`__delitem__`/`__contains__`) |
| 06 | `functools` (`@memo`/reduce/partial) | 51 | `@property` getter **and setter** |
| 07 | generators (`yield` / `yield from`) | 52 | user decorators (`@trace`, `@repeat(n)`, `functools.wraps`) |
| 08 | closures / decorators / HOFs | 53 | chained comparisons + short-circuit operands |
| 09 | dataclasses / `frozen` / mutable-default | 54 | slicing (steps, slice-assign, `del`, str/tuple) |
| 10 | inheritance / `@property`/`@classmethod`/`@staticmethod` | 55 | `super()` across 3-level inheritance |
| 11 | operator dunders (`__add__`/`__eq__`/`__abs__`…) | 56 | custom `__hash__`/`__eq__` as dict/set keys |
| 12 | `enum` (auto + explicit values) | 57 | nested match patterns (class/OR/mapping-rest) |
| 13 | sealed-union expression evaluator | 58 | augmented assignment (all ops, nested targets) |
| 14 | `Result` / `?` / combinators | 59 | graph BFS / DFS |
| 15 | PEP 695 generics (classes/methods/fns) | 60 | Dijkstra with `heapq` |
| 16 | structural `interface`s | 61 | recursive-descent calculator parser |
| 17 | exception hierarchies (try/except/finally/from) | 62 | run-length encode / decode |
| 18 | context managers (`@contextmanager` + `__enter__`) | 63 | roman-numeral conversion both ways |
| 19 | `json` (dumps/loads/roundtrip) | 64 | LRU cache (`OrderedDict` + `popitem`) |
| 20 | `math` | 65 | observer / event bus (`Callable` lists) |
| 21 | structural pattern matching (seq/map/class/guards) | 66 | visitor over a `Json` sealed union |
| 22 | sorting with keys / `min`/`max` key | 67 | matrix transpose / recursive determinant |
| 23 | recursion (gcd/ackermann/quicksort/bsearch) | 68 | `Result` `with`-chain + `else err:` |
| 24 | pipes `\|>` + `guard` | 69 | positional-only `/` + keyword-only `*` params |
| 25 | `newtype` IDs + arithmetic | 70 | `NamedTuple` |
| 26 | `freeze let` deep-immutable bindings | 71 | infinite-generator pipelines + `islice` |
| 27 | `comptime` constants + `comptime def` | 72 | `complex` numbers (mandelbrot escape) |
| 28 | `re` (findall/search/sub/split/match) | 73 | string methods (partition/translate/casefold/…) |
| 29 | string algorithms (wordcount/palindrome/caesar) | 74 | string literals (raw/unicode/escape/triple) |
| 30 | numeric algorithms (sieve/matmul) | 75 | `enum` methods + `IntEnum` / `StrEnum` |
| 31 | sealed-union state machine + guards | 76 | `assert` statements |
| 32 | generic linked list (recursive ADT) | 77 | `zip` / `enumerate` (start, multi, unzip) |
| 33 | `Decimal` / `Fraction` | 78 | deep recursion + bigint |
| 34 | `bytes` / `bytearray` | 79 | `format()` builtin + nested format specs |
| 35 | set algebra + `frozenset` | 80 | typing `Literal`/`Optional`/`Union`/type alias |
| 36 | `model` (Pydantic) validation | 81 | dataclass `__post_init__` + field defaults |
| 37 | walrus `:=` + flow narrowing | 82 | custom exceptions (field-carrying + `__str__`) |
| 38 | `async` / `gather:` / `asyncio.gather` | 83 | context-manager exception suppression |
| 39 | `try_result` combinator | 84 | `datetime` / `date` / `timedelta` |
| 40 | dict views / `|` merge / `setdefault` | 85 | positional match patterns (`__match_args__`) |
| 41 | `*args` / `**kwargs` / unpack-call | 86 | `map` / `filter` / `reduce` |
| 42 | tuple/star/typed unpacking, swap | 87 | bit manipulation |
| 43 | advanced f-strings (nested/`=`/`!r`/specs) | 88 | group-by / aggregate data pipeline |
| 44 | `extend str:` / `extend list:` | 89 | `frozen` + `dataclasses.replace` |
| 45 | `lazy import` | 90 | nested-aggregate builtins (`sum`/`max`/`all`/`any`) |

| # | Area | # | Area |
|---|---|---|---|
| 91 | multiple inheritance / mixins (`plain class`) | 96 | nested/`with a, b:` CMs + `ExitStack` + `suppress` |
| 92 | `abc.ABC` + `@abstractmethod` | 97 | `except (A, B)` tuple + nested re-raise |
| 93 | `ClassVar` class-level state | 98 | async generators / `async for` / `async with` |
| 94 | reflected operators (`__rmul__`, `__lt__`…) | 99 | `functools.singledispatch` |
| 95 | callable objects (`__call__`) as `Callable` | 100 | `io.StringIO` / `hashlib` / `base64` |

| # | Area | # | Area |
|---|---|---|---|
| 101 | `operator` module (`itemgetter`/`attrgetter`/…) | 106 | `__getattr__` / `__setattr__` dynamic record |
| 102 | `bisect` | 107 | `Flag` / `IntFlag` enums |
| 103 | `for`/`while`-`else` + `break`/`continue` | 108 | `TypedDict` |
| 104 | printf-style `%` formatting (tuple + dict) | 109 | `functools.cached_property` |
| 105 | dynamic `getattr`/`setattr`/`hasattr` | 110 | nested / starred unpacking + `**`-call |

| # | Area | # | Area |
|---|---|---|---|
| 111 | generic `interface[T]` (Protocol) | 116 | itertools advanced (`chain.from_iterable`/`tee`/`pairwise`/`batched`) |
| 112 | nested generics (`Box[Box[int]]`, `dict[str, list[int]]`) | 117 | `ChainMap` + `Counter` arithmetic |
| 113 | stacked decorators | 118 | `typing.cast` + `functools.partial` (kwargs) |
| 114 | `@classmethod` alternative constructors | 119 | complex match (`as`/`\|`/nested/guards) |
| 115 | `dataclasses.asdict`/`astuple`/`fields` | 120 | generic recursive tree `fold` |

| # | Area | # | Area |
|---|---|---|---|
| 121 | custom `__format__`/`__bool__`/`__index__`/`__hash__` | 126 | `enum` + methods + `match` + `Weekday(n)` lookup |
| 122 | `__getitem__`/`__setitem__` with tuple keys (Matrix) | 127 | `asyncio.Queue` + `Lock` + `gather:` |
| 123 | rich container (`__iter__`/`__reversed__`/`__contains__`) | 128 | `Decimal` context / `quantize` / rounding |
| 124 | `typing.Self` fluent builder | 129 | `struct` pack / unpack |
| 125 | generic `Either[L,R]` with `map` | 130 | `string.Template` / `format_map` / `textwrap` |

## VM-divergence findings (secondary — production path is correct in all cases)

The 37 `{VM-DIVERGE}` programs are **VM-only** (`tyc run`) gaps; the shipped
Python is correct for every one. Categorised:

### Fixed this round

- **`str.isupper()` / `str.islower()` returned `True` for uncased strings**
  (repro `29`). The VM computed `isupper` as "non-empty and no lowercase
  char", so `",".isupper()`, `" ".isupper()`, `"5".isupper()` all returned
  `True` (CPython: `False` — the predicate requires *at least one cased
  character*). This silently corrupted a Caesar-cipher round-trip under
  `tyc run`. **Fixed** in `tyc-vm/src/builtins.rs`; repro `29` no longer
  diverges. The compiled path was always correct.

### Documented VM limitations (run these with `tyc build` / `tyc run --compile`)

- `18`, `83`, `98` — `@contextmanager` / `@asynccontextmanager` generators as
  context managers (the VM evaluates generators eagerly — the explicit error
  the VM raises).
- `33`, `100`, `101`, `102`, `128`, `129`, `130` — `decimal` / `fractions` /
  `io` / `operator` / `bisect` / `struct` / `string` not in the VM's native
  stdlib subset (the explicit "run with `tyc run --compile`" error).
- `71` — unbounded generators (`while True: yield`) exceed the VM's
  1M-value eager-materialisation cap.
- `78` — `sys.setrecursionlimit` not modelled by the VM.

### VM stdlib/operator coverage gaps (candidates for a future VM round)

- `04` — `Counter[missing_key]` raises `KeyError` instead of returning `0`.
- `05` — `itertools.product(..., repeat=N)` keyword unsupported.
- `06` — `functools.cmp_to_key` missing.
- `34` — `bytearray` item assignment (`ba[0] = 65`) path errors.
- `35` — set subset/superset comparisons (`a <= b`, `a < b`) unsupported.
- `40` — `dict | dict` merge operator unsupported.
- `52` — `functools.wraps` does not propagate `__name__` (VM prints the
  wrapper's name; CPython prints the wrapped function's).
- `70` — a `NamedTuple` instance is not subscriptable / unpackable as a tuple
  in the VM (`p[0]`, `let (a, b) = p`).
- `75` — `int(IntEnum.MEMBER)` raises in the VM (IntEnum member not coerced).
- `84` — `date.weekday()` and several `date` ops missing from the VM shim.
- `89` — `dataclasses.replace` not implemented in the VM.
- `91` — `type.__mro__` not exposed by the VM.
- `96` — `contextlib.suppress` not implemented in the VM.
- `99` — `functools.singledispatch` not implemented in the VM.
- `104` — printf-style `%` with a **mapping** (`"%(name)s" % {...}`) unsupported.
- `107` — `Flag`/`IntFlag` membership (`x in flag`) treats the flag as a bare int.
- `108` — a `TypedDict`-typed value is not subscriptable in the VM (`m["k"]`).
- `109` — `functools.cached_property` re-computes (no per-instance cache) in the VM.
- `115` — `dataclasses.astuple` not implemented in the VM (`asdict` is).
- `116` — `itertools.chain.from_iterable` not exposed in the VM.
- `117` — `collections.ChainMap` not implemented in the VM.
- `118` — `functools.partial` rejects keyword arguments in the VM.
- `127` — `asyncio.Lock` not implemented in the VM.

### Cosmetic (different repr, same value)

- `26` — frozen dict reprs as `mappingproxy({...})` vs `{...}`.
- `36` — VM `model_dump_json()` uses default `json` separators where Pydantic
  emits compact `,`/`:`.

## Files

- `repros/*.ty` — the 130-program corpus.
- `harness.sh` — three-path runner + comparator.
- `results.txt` — captured full-sweep output (post-fix).
