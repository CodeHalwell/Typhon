# Stress-test corpus — 2026-06-21 (v0.15.6)

**Goal:** confirm that a broad cross-section of "things you actually write in
Python" expressed as Typhon compiles to **valid, runnable Python**. Python can
express almost anything, so the corpus deliberately spans many independent
language surfaces and lowering paths rather than going deep on one.

## Methodology

Each of the 48 `.ty` programs in `repros/` is run through three paths by
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

## Result — 48 / 48 compile to valid, runnable Python

```
total=48 pass=48 buildfail=0 runfail=0 checkfail=0 vm_diverge=10
```

Every program type-checks, builds, and runs correctly on the compiled
(`tyc build` → CPython 3.13) path. **Zero codegen defects** were found across
the whole corpus.

## Coverage (48 programs)

| # | Area | # | Area |
|---|---|---|---|
| 01 | arithmetic, bigint, complex, bases | 25 | `newtype` IDs + arithmetic |
| 02 | string methods + f-string formatting | 26 | `freeze let` deep-immutable bindings |
| 03 | list/dict/set comprehensions, nested | 27 | `comptime` constants + `comptime def` |
| 04 | `collections` (deque/Counter/defaultdict/OrderedDict) | 28 | `re` (findall/search/sub/split/match) |
| 05 | `itertools` | 29 | string algorithms (wordcount/palindrome/caesar) |
| 06 | `functools` (`@memo`/reduce/partial) | 30 | numeric algorithms (sieve/matmul) |
| 07 | generators (`yield` / `yield from`) | 31 | sealed-union state machine + guards |
| 08 | closures / decorators / HOFs | 32 | generic linked list (recursive ADT) |
| 09 | dataclasses / `frozen` / mutable-default | 33 | `Decimal` / `Fraction` |
| 10 | inheritance / `@property`/`@classmethod`/`@staticmethod` | 34 | `bytes` / `bytearray` |
| 11 | operator dunders (`__add__`/`__eq__`/`__abs__`…) | 35 | set algebra + `frozenset` |
| 12 | `enum` (auto + explicit values) | 36 | `model` (Pydantic) validation |
| 13 | sealed-union expression evaluator | 37 | walrus `:=` + flow narrowing |
| 14 | `Result` / `?` / combinators | 38 | `async` / `gather:` / `asyncio.gather` |
| 15 | PEP 695 generics (classes/methods/fns) | 39 | `try_result` combinator |
| 16 | structural `interface`s | 40 | dict views / `|` merge / `setdefault` |
| 17 | exception hierarchies (try/except/finally/from) | 41 | `*args` / `**kwargs` / unpack-call |
| 18 | context managers (`@contextmanager` + `__enter__`) | 42 | tuple/star/typed unpacking, swap |
| 19 | `json` (dumps/loads/roundtrip) | 43 | advanced f-strings (nested/`=`/`!r`/specs) |
| 20 | `math` | 44 | `extend str:` / `extend list:` |
| 21 | structural pattern matching (seq/map/class/guards) | 45 | `lazy import` |
| 22 | sorting with keys / `min`/`max` key | 46 | mutual recursion + binary tree |
| 23 | recursion (gcd/ackermann/quicksort/bsearch) | 47 | `global` / module-level `mut` state |
| 24 | pipes `\|>` + `guard` | 48 | `as!` checked boundary cast |

## VM-divergence findings (secondary — production path is correct in all cases)

The 10 remaining `{VM-DIVERGE}` programs are **VM-only** gaps; the shipped
Python is correct for every one. Categorised:

### Fixed this round

- **`str.isupper()` / `str.islower()` returned `True` for uncased strings**
  (repro `29`). The VM computed `isupper` as "non-empty and no lowercase
  char", so `",".isupper()`, `" ".isupper()`, `"5".isupper()` all returned
  `True` (CPython: `False` — the predicate requires *at least one cased
  character*). This silently corrupted a Caesar-cipher round-trip under
  `tyc run` (`"Hello, World!"` → `"KhoorIWZruogX"`). **Fixed** in
  `tyc-vm/src/builtins.rs`: both predicates now require a cased character of
  the matching case and none of the opposite case. The compiled path was
  always correct; repro `29` no longer diverges.

### Documented VM limitations (run these with `tyc build` / `tyc run --compile`)

- `18` — `@contextmanager` generators as context managers (the VM evaluates
  generators eagerly; this is the explicit error the VM raises).
- `33` — `decimal` / `fractions` are not in the VM's native stdlib subset.

### VM stdlib/operator coverage gaps (candidates for a future VM round)

- `04` — `Counter[missing_key]` raises `KeyError` instead of returning `0`.
- `05` — `itertools.product(..., repeat=N)` keyword unsupported.
- `06` — `functools.cmp_to_key` missing.
- `34` — `bytearray` item assignment (`ba[0] = 65`) path errors.
- `35` — set subset/superset comparisons (`a <= b`, `a < b`) unsupported.
- `40` — `dict | dict` merge operator unsupported.

### Cosmetic (different repr, same value)

- `26` — frozen dict reprs as `mappingproxy({...})` vs `{...}`.
- `36` — VM `model_dump_json()` uses default `json` separators where Pydantic
  emits compact `,`/`:`.

## Files

- `repros/*.ty` — the 48-program corpus.
- `harness.sh` — three-path runner + comparator.
- `results.txt` — captured full-sweep output (post-fix).
