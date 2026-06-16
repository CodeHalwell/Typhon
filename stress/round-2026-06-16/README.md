# Stress-test corpus — 2026-06-16 (v0.15.5)

Adversarial "can a real Python developer use this?" sweep. Methodology: a
38-program corpus (`repros/`) spanning the breadth of what production Python
apps actually do — recursion/generators, closures/decorators, operator
dunders, `collections`/`itertools`/`functools`, numeric (`math`/`Fraction`/
`Decimal`), strings/regex/f-string formatting, exceptions, context managers,
inheritance/properties/`classmethod`/`staticmethod`, ABCs, protocols,
`NamedTuple`, `enum`, dataclasses, JSON, comprehensions, sealed-union state
machines / trees, `Result` pipelines, generics. Each program is run through
three paths and compared:

1. `tyc check`          — frontend (parser + type checker)
2. `tyc build` + `python3.13`  — codegen / production path
3. `tyc run`            — in-process VM

`harness.sh` automates paths 2 and 3 and diffs VM vs CPython output.

## Headline finding (FIXED) — custom exceptions broke the *production* path

`class FooError(Exception): pass` emitted `@dataclasses.dataclass(slots=True)`,
whose synthesised no-arg `__init__` shadows `BaseException.__init__`, so the
single most common custom-error idiom —

```python
raise FooError("message")
```

— died at runtime with `TypeError: FooError.__init__() takes 1 positional
argument but 2 were given`, on **both** the compiled output and the VM. This
hit every exception subclass (builtin bases `Exception`/`ValueError`/`KeyError`/
`Warning`/…, and user hierarchies like `AppError` → `NotFoundError`). It was
the only divergence where the *shipped* Python was wrong.

**Fix (`tyc-desugar`):** exception subclasses are detected by the same
name-based heuristic the existing skip-decoration list uses (base segment ends
in `Error`/`Exception`/`Warning`, or is an exact non-suffixed builtin like
`BaseException`/`KeyboardInterrupt`), and routed through the same lowering as
`class!`: no `@dataclass` decorator, and a `super().__init__(...)`-calling
constructor synthesised only when the body declares fields (a field-less body
stays bare and inherits `BaseException.__init__`). Error-named classes with **no
base** (Result error *variants*, ubiquitous in the corpus and examples) are
untouched and keep their dataclass shape.

## VM exception parity (FIXED) — `tyc run` matched the production path

The VM independently mis-modelled exception subclasses. Fixed in `tyc-vm`:

- **Construction** — a field-less exception now constructs `BaseException`-style
  (stashes positional args as `.args`) instead of "takes 0 arguments".
- **`str()` / `repr()`** — render from `.args` (`str(FooError("x")) == "x"`,
  `repr == "FooError('x')"`), not the dataclass field form.
- **`except` matching** — `except KeyError` now catches a user
  `class MyKeyError(KeyError):` (builtin exception bases are recorded on the
  class since the VM has no `Value::Class` for them).
- **`super().__init__(msg)`** in a hand-written exception `__init__` is captured
  so `str(e)` reflects the custom message.
- **Missing builtin exceptions** added: `BaseException`, `Warning` (+ subclasses),
  `NameError`, `ImportError`, `UnicodeError`, `ConnectionError`, `IOError`,
  `KeyboardInterrupt`/`SystemExit`/`GeneratorExit`, etc.

## Second sweep — batches F–K (~50 more programs)

A deeper round probing advanced OOP (custom `__iter__`/`__len__`/`__getitem__`,
property setters, MRO), slicing, `*args`/`**kwargs`/keyword-only, `match`
pattern edge cases, unicode/bytes, decorators-with-args, async `gather:`,
mutual recursion, deep narrowing, bignum, generics, and real algorithm-shaped
programs (RPN calculator, tokenizer, LRU cache, BFS, event emitter). Three
more fixes:

- **`tyc::missing_return` false positive on nested class patterns (production
  blocker, FIXED).** An exhaustive `match` like
  `case Circle(center=Point(x=cx, y=cy), radius=r):` (where `center: Point`)
  was rejected because the nested `Point(...)` sub-pattern wasn't a bare
  capture — **blocking the build** on valid, idiomatic code. The checker now
  recognises a nested class sub-pattern as total when it covers the field's
  exact type; a nested *value* filter (`x=0`) stays refutable.
- **`**kwargs` call-order (VM, FIXED).** `**kwargs` was collected into a
  `HashMap`, so the dict's order was nondeterministic; now insertion-ordered.
- **`bytes` `+` / `*` (VM, FIXED).** `b"a" + b"b"` and `b"a" * 3` now work
  under `tyc run`.

The one genuine *test* error found was mine: declaring `let first` then
rebinding it via `[first, *rest] = ...` correctly trips `immutable_assign` —
the checker is right.

## Third sweep — batches L–O (~30 more programs)

Custom hashables, fluent interfaces, dict-view set algebra, `match` on tuples
of unions and on list shapes, OR-patterns, guard cascades, generic memoize,
and real programs (KV store, matrix multiply, inventory, Caesar cipher, RPN).
Three more **production-blocking checker false-positives** fixed — all
`tyc::missing_return` / `operator_type_mismatch` on valid, idiomatic code:

- **`d1.keys() - d2.keys()`** (dict-view set difference) was rejected; the
  set-difference carve-out only knew `set`/`frozenset`. Extended to the
  set-like views, and taught the VM to evaluate them.
- **`match (cmd, count):`** over a `tuple[Union, int]` (state-machine
  dispatch) had no product-coverage analysis, so an exhaustive match was
  rejected. Added a sound column-wise `tuple_cases_cover`.
- **`case [first, *middle, last]:`** — the list-length coverage check only
  accepted a *tail* star, so an exhaustive list match false-fired. Generalised
  to a star at any position.

## Fourth sweep — batches P–U (~48 more programs)

Async iteration / context managers / gather, Literal types, overload-shaped
`isinstance` chains, bounded generics, stacked decorators, `__call__` /
`__format__` instances, `del` slices, custom hashables, deep narrowing, and
real programs (graph BFS, state machines, recursive evaluators, linked-list
drains). Findings:

**Production-blocking checker false-positives (fixed):**
- **`match s:` after `if s is None: return`** keyed off the *declared* type
  (`Shape?`), so it thought `None` was uncovered. Now uses the *narrowed* type.
- **Optional attribute narrowing** — `if self.value is None: return …` didn't
  narrow `self.value`, so "check an optional field, then use it" (one of the
  most common Python patterns) false-fired. Added flow-sensitive narrowing
  keyed by access path, with assignment-narrowing and full soundness scoping.

**VM-only (compiled already correct, fixed for `tyc run` parity):**
- `del lst[i:j]` / `del lst[::k]` slice deletion.
- User `__format__(self, spec)` dispatch from `f"{x:spec}"` / `.format` /
  `format()`.
- `str(KeyError("k"))` → `"'k'"`.

A couple of apparent findings were correct behavior (my test errors): a
`list[dict] → list[object]` assignment is a real invariance violation, and a
coroutine list annotated `list[object]` likewise.

## Result

The **production path (`tyc build` → CPython 3.13) is correct on the entire
166-program corpus** (0 build/runtime failures). The frontend held up with
zero remaining false positives. Remaining divergences are all VM-only with
correct compiled output, and are documented VM-coverage limitations:

| Repro | VM gap (compiled path correct) |
|-------|--------------------------------|
| a06 | VM has no `fractions` / `decimal` (errors gracefully → `--compile`) |
| b04 | VM `@contextmanager` generators as `with` targets (errors gracefully) |
| c03 | VM `NamedTuple` indexing / `_replace` |
| d01 | VM has no `abc` import (errors gracefully) |
| d04 | VM `os.path.splitext` |
| d05 | VM `date.weekday()` |
| e01 | VM does not track `__cause__` (`raise X from Y` introspection) |
| e02 | CPython's `KeyError.__str__` repr-quoting quirk |
| f08 | VM nested format-spec width (`{n:>{w}}`) |
| i08 | VM has no `contextlib.suppress` |
| k04 | VM has no `bytearray` type |

Regression: full `cargo test --workspace` green (41 suites); zero `tyc check`
regressions across all `examples/` exercises and `examples/apps/`;
representative apps build and run under CPython 3.13.
