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

## Result

The **production path (`tyc build` → CPython 3.13) is correct on all 38
programs.** The frontend held up with zero false positives. Remaining
divergences are all VM-only with correct compiled output, and are documented
VM-coverage limitations:

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

Regression: full `cargo test --workspace` green; zero `tyc check` regressions
across all `examples/` exercises and `examples/apps/`; representative apps build
and run under CPython 3.13.
