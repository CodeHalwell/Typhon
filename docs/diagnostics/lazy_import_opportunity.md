# tyc::lazy_import_opportunity

Advice-level diagnostic, **on by default** (part of the `tyc::perf_*` family,
gated by `[strictness] suggest-perf`). Fires on a module-level `import X` /
`import X as Y` whose bound name is referenced *only* inside function / method
bodies — never at module scope, in an annotation, a decorator, or a re-export.
Declaring it `lazy import` defers that module's import cost from process
startup until the first call that actually needs it.

## Example

```ty
import numpy as np                    # advice: `np` is only used inside functions

def embed(text: str) -> object:
    return np.asarray(tokenize(text))
```

## Why

A top-level `import` runs when the module is first loaded — every importer pays
the cost even if they never call the function that needs the dependency. Heavy
scientific / SDK packages (numpy, pandas, torch, boto3, …) can dominate startup
time. `lazy import np = numpy` builds a thread-safe on-first-use proxy, so the
real import happens only when `np` is first touched at runtime.

On a Python **3.15** target Typhon lowers this to a native
[PEP 810](https://peps.python.org/pep-0810/) lazy import.

The diagnostic is **advice**, never an error, and the compiler never rewrites
the code — deferring an import can move an `ImportError` to a later point, so
it's the author's call.

## When it does *not* fire

Deliberately conservative — it only nudges imports where the win is real and
safe:

- **stdlib** modules (deferring `os` / `json` buys nothing and risks shadowing
  an already-loaded module);
- the name is used **eagerly** anywhere — at module scope, in a decorator, or
  in a parameter / return **annotation** (all evaluated at import time, so a
  lazy import would break them);
- the module is shaped like an executable script — an `if __name__ ==
  "__main__":` guard or a top-level `def main()` — whose imports load when it
  runs, so deferral gives no startup win (the lint's real target is a *library*
  module, imported by others);
- the name is **re-exported** (`pub`, in a hand-written `__all__`, or the file
  has a `pub *`), or the file is an `__init__`;
- the import is **already** a `lazy import`.

## Fix

Make it lazy:

```ty
lazy import np = numpy

def embed(text: str) -> object:
    return np.asarray(tokenize(text))
```

Note `lazy from numpy import asarray` is rejected — deferral needs the module
object, so use `lazy import np = numpy` and dotted access.

Silence the whole family project-wide with `[strictness] suggest-perf = false`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/lazy_import_opportunity.md
