# Third-party introspection — wide adversarial audit (2026-06-21)

Deep, adversarial audit of Typhon's venv signature introspection
(`tyc-venv` → `tyc-types`) across **43 popular third-party libraries**
spanning numeric, ML, web, HTTP, CLI, validation, DB, async, viz, cloud and
utility use cases. Toolchain: `tyc 0.15.6`, CPython 3.13.12, libraries
installed into a real project `.venv` (versions in `requirements.txt`).

## Executive summary

- **False-positive rate: 0/44 must_pass (0.0%).** Every idiomatic,
  correct program — constructors, free functions, fluent method chains,
  keyword args, decorators, nested-module access, the implicit-Optional
  idiom — type-checks clean. The conservative "degrade to `Unknown`" design
  holds across pure-Python, C-extension, stub-only and `**kwargs`/`Annotated`
  APIs.
- **Coverage by call kind (must_fail, post-fix): 16/16 caught** —
  constructors (`Flask`, `Pipeline`, `uvicorn.Config`, `django.conf.Settings`),
  free functions (`create_engine`, `dateutil.parser.parse`), and **methods**
  (`scaler.transform`, `pca.fit`, `schema.load`, `redis.set`), across
  `missing_argument`, `type_mismatch` and `unknown_kwarg`.
- **Three real bugs found and fixed**, all surfaced only by going wide:
  1. **Introspection of an entire module silently crashed** when any
     member raised a non-`(TypeError|ValueError)` from `inspect.signature`
     (or `callable()`) — the canonical trigger is a re-exported proxy
     object (Flask's `current_app`/`g`/`request`/`session`; Django's
     `settings`). All of Flask's third-party checks were silently disabled
     (a false negative), and the crash is pervasive — it affects **12 flask
     submodules**, since the proxies leak into nearly every one via
     `from .globals import …`.
  2. **Multi-segment attribute calls** (`pkg.sub.Thing()`) skipped the
     check that the `from`-import form (`from pkg.sub import Thing; Thing()`)
     already got — a pure false negative on `sklearn.pipeline.*`,
     `django.conf.*`, `dateutil.parser.*`, `rich.console.*`, `starlette.applications.*`.
  3. **The implicit-Optional idiom (`x: int = None`) produced a real
     `type_mismatch` FALSE POSITIVE** — the critical class. A param whose
     annotation is a bare scalar but whose default is `None` "lies": `None`
     (and any nullable value) is valid, but the checker rejected it. Observed
     on real libraries — `redis.exceptions.AskError`/`MovedError`/
     `ClusterDownError`/`MasterDownError` (`status_code: str = None`) and
     `pydantic.v1.confloat`/`conlist`/`parse_file_as`/`parse_raw_as`. Fixed
     by widening a None-default param's type to nullable.
- **Baseline vs. fixed:** the same corpus on the pre-fix `tyc 0.15.6`
  binary missed **7/16** must_fail cases **and false-positive-rejected the
  implicit-Optional must_pass** (`AskError("resp", status_code=None)` →
  `expected str, found None`). The fixes recover all 7 missed checks and the
  false positive, introduce **no new false positives**, and keep the full
  workspace suite + the 130/130 repros corpus green.
- **Top 3 to improve next:** (1) unwrap `typing.Annotated[T, …]` to `T`
  so FastAPI/Typer/Pydantic kw-only param *types* become checkable;
  (2) model the common 2-member `Union[str, bytes]`/`Union[str, PathLike]`
  forms instead of degrading the whole annotation to `Unknown`;
  (3) lean on the `[checker] external = "ty"` typeshed pass for
  C-extension libs (numpy/pandas/scipy/pyarrow/lxml/pillow), which
  introspection structurally cannot see.

## Library matrix (43 libraries)

API style legend: **ctor** = class with `__init__`; **fn** = free
functions; **method** = instance methods; **kw**/`**kwargs` = keyword/var-kw;
**deco** = decorator-driven; **C-ext** = compiled (no Python signature);
**stub** = typed via typeshed/`.dty` not inline.

| # | Library (dist→import) | Version | API style | introspectable? | must_pass | must_fail | FP | missed |
|---|---|---|---|---|---|---|---|---|
| 1 | numpy | 2.4.6 | C-ext | no (lenient) | 1 | – | 0 | – |
| 2 | pandas | 3.0.3 | C-ext/Cython | no (lenient) | 1 | – | 0 | – |
| 3 | scipy | 1.18.0 | C/Fortran-ext | no (lenient) | 1 | – | 0 | – |
| 4 | polars | 1.41.2 | Rust-ext | no (lenient) | 1 | – | 0 | – |
| 5 | pyarrow | 24.0.0 | C++-ext | no (lenient) | 1 | – | 0 | – |
| 6 | scikit-learn→sklearn | 1.9.0 | ctor+method | **yes** | 1 | 3 | 0 | 0 |
| 7 | xgboost | 3.3.0 | `**kwargs` ctor | partial (lenient) | 1 | – | 0 | – |
| 8 | lightgbm | 4.6.0 | `**kwargs` ctor | partial (lenient) | 1 | – | 0 | – |
| 9 | fastapi | 0.138.0 | `Annotated` kw | partial (lenient) | 1 | – | 0 | – |
| 10 | flask | 3.1.3 | ctor+method | **yes** ⚠fixed | 1 | 3 | 0 | 0 |
| 11 | starlette | 1.3.1 | ctor (nested) | **yes** ⚠fixed | 1 | 1 | 0 | 0 |
| 12 | pydantic | 2.13.4 | `model`/BaseModel | partial | 1 | – | 0 | – |
| 13 | uvicorn | 0.49.0 | ctor | **yes** | 1 | 1 | 0 | 0 |
| 14 | requests | (latest) | stub/bundled .dty | bundled | 1 | – | 0 | – |
| 15 | httpx | 0.28.1 | bundled .dty+ctor | bundled | 1 | – | 0 | – |
| 16 | aiohttp | 3.14.1 | ctor+method | partial | 1 | – | 0 | – |
| 17 | urllib3 | 2.7.0 | ctor | partial | 1 | – | 0 | – |
| 18 | click | 8.4.1 | deco | lenient | 1 | – | 0 | – |
| 19 | typer | 0.26.7 | `Annotated` kw | partial (lenient) | 1 | – | 0 | – |
| 20 | rich | 15.0.0 | ctor (nested) | **yes** ⚠fixed | 1 | 1 | 0 | 0 |
| 21 | python-dotenv→dotenv | 1.2.2 | fn | yes | 1 | – | 0 | – |
| 22 | marshmallow | 4.3.0 | ctor+method | **yes** | 1 | 1 | 0 | 0 |
| 23 | attrs→attr | 26.1.0 | deco | lenient | 1 | – | 0 | – |
| 24 | msgspec | 0.21.1 | C-ext | no (lenient) | 1 | – | 0 | – |
| 25 | orjson | 3.11.9 | C-ext | no (lenient) | 1 | – | 0 | – |
| 26 | sqlalchemy | 2.0.51 | fn+ctor | **yes** | 1 | 1 | 0 | 0 |
| 27 | redis | 8.0.0 | ctor+method | **yes** ⚠fixed | 2 | 2 | 0 | 0 |
| 28 | pymongo | 4.17.0 | `**kwargs` ctor | partial (lenient) | 1 | – | 0 | – |
| 29 | psycopg2-binary→psycopg2 | 2.9.12 | C-ext | no (lenient) | 1 | – | 0 | – |
| 30 | anyio | 4.14.0 | fn | yes | 1 | – | 0 | – |
| 31 | trio | 0.33.0 | fn | yes | 1 | – | 0 | – |
| 32 | matplotlib | 3.11.0 | `*args` fn | lenient | 1 | – | 0 | – |
| 33 | seaborn | 0.13.2 | fn | partial | 1 | – | 0 | – |
| 34 | plotly | 6.8.0 | ctor (nested) | partial | 1 | – | 0 | – |
| 35 | pillow→PIL | 12.2.0 | C-ext | no (lenient) | 1 | – | 0 | – |
| 36 | beautifulsoup4→bs4 | 4.15.0 | `**kwargs` ctor | partial (lenient) | 1 | – | 0 | – |
| 37 | lxml | 6.1.1 | C-ext | no (lenient) | 1 | – | 0 | – |
| 38 | pyyaml→yaml | 6.0.3 | fn | yes | 1 | – | 0 | – |
| 39 | jinja2 | 3.1.6 | ctor+method | **yes** | 1 | 1 | 0 | 0 |
| 40 | python-dateutil→dateutil | 2.9.0 | fn (nested) | **yes** ⚠fixed | 1 | 1 | 0 | 0 |
| 41 | django | 6.0.6 | ctor (nested) | **yes** ⚠fixed | 1 | 1 | 0 | 0 |
| 42 | boto3 | 1.43.34 | dynamic factory | lenient | 1 | – | 0 | – |
| 43 | google-cloud-storage→google | 3.12.0 | namespace pkg | lenient | 1 | – | 0 | – |

`⚠fixed` = a library whose checks were previously broken — a false negative
(flask/django/dateutil/rich/starlette) or a false positive (redis) — and are
corrected by the fixes below. Totals: **44 must_pass / 16 must_fail / 0 false
positives / 0 missed (post-fix)**.

The import-name≠dist-name resolution was exercised and works for every
case: `scikit-learn→sklearn`, `pillow→PIL`, `beautifulsoup4→bs4`,
`pyyaml→yaml`, `python-dateutil→dateutil`, `python-dotenv→dotenv`,
`attrs→attr`, `psycopg2-binary→psycopg2`, `google-cloud-storage→google`.

---

## Ranked issues

### #1 — CRITICAL (false negative, build-blocker-silencing): one proxy member crashes a whole module's introspection — *FIXED*

**Severity:** high. A single re-exported object that raises an unexpected
exception from `inspect.signature` / `callable()` silently disabled **all**
third-party checks for the library — indistinguishable from a clean pass,
the most dangerous failure mode this feature has.

**Minimal repro** (`must_fail/01-flask-missing-import-name.ty`):

```python
import flask
def main() -> None:
    let app: flask.Flask = flask.Flask()   # missing required `import_name`
    print(app)
```

Pre-fix: `tyc check` reports no error; at runtime
`TypeError: Flask.__init__() missing 1 required positional argument: 'import_name'`.

**Root cause** (`tyc-venv/src/lib.rs`, `INTROSPECT_SCRIPT`): Flask
re-exports werkzeug `LocalProxy` objects (`current_app`, `g`, `request`,
`session`) at module scope. `callable(proxy)` is `True`, so `kind_of`
labels them `"function"` and `params_of` calls `inspect.signature(proxy)`,
which raises `RuntimeError: Working outside of application context`.
`params_of`/`returns_of` only caught `(TypeError, ValueError)`, so the
`RuntimeError` propagated out of `introspect_one`, crashed the subprocess
(non-zero exit), and the batch (then the per-module fallback) recorded
flask as an empty miss. Every flask class/function went unchecked. Django's
`django.conf.settings` (`LazySettings`) trips the same path via `callable()`
inside `kind_of`.

**Fix:** broaden `params_of`/`returns_of` to catch any `Exception` (treat
as "no signature recoverable" → skip the member, stay lenient), and wrap
the per-member body of `introspect_one` in a `try/except BaseException` so
a pathological member can only lose itself, never the module. Strictly
widens robustness — recovered shapes are the same `inspect`-derived,
conservatively-modelled ones already trusted for jinja2/sklearn, so it can
only *add* true positives. Regression test
`introspection_survives_a_member_that_raises_on_signature`.

### #2 — HIGH (false negative): multi-segment attribute calls skip the check — *FIXED*

**Severity:** high. `pkg.sub.Thing()` / `pkg.sub.func()` silently skipped
the arity/type check that `from pkg.sub import Thing; Thing()` already got.
Affected every nested-module access — extremely common idiom.

**Minimal repro** (`must_fail/07-dateutil-parse-missing-timestr.ty`):

```python
import dateutil.parser
def main() -> None:
    let dt = dateutil.parser.parse()   # missing required `timestr`
    print(dt)
```

Pre-fix: no error; `from dateutil.parser import parse; parse()` *was*
caught. Same split for `sklearn.pipeline.Pipeline()`,
`django.conf.Settings()`, `rich.console.Console(width="wide")`,
`starlette.applications.Starlette(bogus=1)`.

**Root cause** (`tyc-types/src/lib.rs`): `import pkg.sub` binds the *top*
name `pkg` (Python semantics), so the resolver's `ImportInfo.module` is
`pkg` — but venv enrichment only introspects and registers the imported
*leaf* `pkg.sub`. Three spots conspired: (a) the `BindingKind::Import` arm
required `module_registry.contains_key("pkg")` to give `pkg` a
`Type::Module`, but only `"pkg.sub"` is a key → `pkg` became `Unknown`;
(b) the `Type::Module` arm of `infer_attribute` returned `Unknown` for a
nested submodule attribute instead of chaining to `Type::Module("pkg.sub")`;
(c) the side-effect-free `infer_expr_readonly` Module arm (used to compute
the call-site `fn_name` for the function-arity lookup) had the same two
gaps. All three now recognise a parent-of-a-registered-key as a module and
chain nested submodule access. Regression test
`nested_submodule_attribute_constructor_arity_checks`.

### #3 — CRITICAL (false positive): the implicit-Optional idiom (`x: T = None`) wrongly rejected — *FIXED*

**Severity:** high — this is the worst class (a false positive is a
build-blocker on valid library code). A parameter annotated with a bare
scalar but defaulted to `None` ("implicit Optional", pervasive in Python)
was type-checked against the non-nullable scalar, so passing `None` — or any
nullable value — was rejected.

**Minimal repro** (`must_pass/44-redis-implicit-optional.ty`, a *valid*
program the pre-fix compiler rejected):

```python
from redis.exceptions import AskError    # AskError(resp, status_code: str = None)
def main() -> None:
    let e: AskError = AskError("MOVED 1 127.0.0.1:7001", status_code=None)
    print(e)
```

Pre-fix: `tyc::type_mismatch: expected ``str``, found ``None```. Observed on
real installed libraries: `redis.exceptions` (`AskError`, `MovedError`,
`ClusterDownError`, `MasterDownError` — `status_code: str = None`) and
`pydantic.v1` (`confloat`, `conlist`, `parse_file_as`, `parse_raw_as`). (The
base `redis.RedisError` escapes only because it also declares `*args`, which
makes `class_shape_from_params` skip the whole class.)

**Root cause** (`tyc-venv/src/lib.rs`): introspection captured the
annotation faithfully (`str`) but discarded the fact that the default is
`None`, so `annotation_to_type("str") = Str` and the checker rejected
`None`.

**Fix:** capture `default_is_none` per parameter in `INTROSPECT_SCRIPT`, and
in the new `param_type_from` helper widen a None-default param's concrete
type to nullable (`str → str | None`). Only concrete, non-nullable types are
widened — an already-`Optional[X]` annotation, `Unknown`, or `None` is left
untouched. The widening only ever *adds* accepted values, so it can only
remove false positives, never introduce one; a genuinely wrong-typed
argument (`status_code=123`) still fails against `str | None`
(`must_fail/16-implicit-optional-wrong-type.ty`). Regression test
`implicit_optional_default_none_widens_param_to_nullable`.

### #4 — by-design leniency (documented, not bugs)

These are correct conservative behaviour — recorded so they aren't
mistaken for gaps:

- **`**kwargs` constructors stay fully permissive** (BeautifulSoup,
  MongoClient, FastAPI, XGBClassifier, pydantic via subclass). `class_shape_from_params`
  returns `None` on a `*args`/`**kwargs` constructor, so no `missing`/
  `unknown_kwarg` fires. This is the right call — these APIs genuinely
  accept arbitrary kwargs.
- **C-extension libraries are inherently lenient** (numpy, pandas, scipy,
  polars, pyarrow, msgspec, orjson, lxml, pillow, psycopg2). `inspect.signature`
  raises for compiled slots, so shapes are never built — correct, never a
  false positive. Coverage for these needs the typeshed pass
  (`[checker] external = "ty"`), Layer 3.
- **Decorator-wrapped `(*args, **kwargs)` methods stay permissive** (sklearn
  via `functools.wraps`): the receiver-strip logic deliberately does *not*
  strip a leading `*args`, so the method keeps an unbounded arity.

---

## Deferred / design-sensitive (intentionally not fixed)

1. **`typing.Annotated[T, …]` degrades to `Unknown`.** FastAPI/Typer/
   Pydantic annotate kw-only params as `Annotated[str, FieldInfo(...)]`, so a
   wrong-*typed* kwarg (`typer.Typer(name=123)`) isn't caught. Unwrapping
   `Annotated[T, …] → T` in `annotation_to_type` would recover these, but
   needs care (the metadata can carry validators that change the effective
   type) and touches a popular surface — worth a dedicated, separately
   reviewed change. Low risk of *false positive* either way; this is a
   missed-check, not a wrong-rejection.
2. **2-member non-nullable unions degrade to `Unknown`.**
   `Union[str, bytes]` (`jinja2.Template(source)`),
   `Union[str, os.PathLike]` (Flask `static_folder`) etc. are common and
   could be modelled as a real `Type::Union` so an `int` argument is
   rejected. Currently only `X | None` is recognised; richer unions stay
   permissive by design. A targeted relaxation is plausible but must keep
   widening rules sound (e.g. `int→float` inside the union).
3. **Cython/`*.pyi`-typed pure-extension libs** (pandas/numpy public API)
   are out of introspection's reach by construction; the report flags the
   typeshed pass as the path, not an introspection change.

---

## Reproduce

```bash
cd tyc && cargo build --release --bin tyc && cd ..
cd stress/round-2026-06-21/third-party-wide
python3.13 -m venv proj/.venv
proj/.venv/bin/pip install -r requirements.txt
bash harness.sh        # TYC=/path/to/tyc overridable
# Expect: must_pass ok=44 false_positive=0 | must_fail caught=16 missed=0
```

`proj/` (venv + generated build output) is gitignored and must not be
committed. The in-crate `tyc-venv` unit harness does not populate venv
data, so the venv-dependent behaviour above is verified end-to-end against
the built binary; the three landed fixes additionally carry crate-level
regression tests (`tyc-venv` and `tyc-types`).
