# Typhon Runtime and VM

Two things live under "runtime" in Typhon: the **generated `typhon_runtime/` package** that ships alongside emitted Python, and the **in-process tree-walking interpreter** (`tyc-vm`) that powers `tyc run`. Both deserve depth — they're easy to confuse but serve different audiences.

---

## Part 1 — The `typhon_runtime` package

`typhon_runtime/` is a **generated package** the build owns. It is written into the project's output directory on every `tyc build` whenever the desugar pass sets `needs_typhon_runtime = true` — i.e. whenever the emitted code references any of:

- `Ok` / `Err` / `Result` types or pattern matches
- `typhon_runtime.tasks.spawn` (from `go` lowering)
- `typhon_runtime.lazy.lazy_let` (from module-level `lazy let`)
- `typhon_runtime.lazy.lazy_import` (from `lazy import` lowering)
- `typhon_runtime.parallel.map_pure` (from `[strictness] auto-parallel`)
- `typhon_runtime.freeze.deep_freeze` (from `freeze let`)
- `typhon_runtime.cast.checked_cast` (from `EXPR as! TYPE`) — v0.14.0
- `typhon_runtime.traceback.install` (from `[emit] traceback-remap = true`) — v0.14.0
- `typhon_runtime.try_result` (from a `try_result(...)` call) — v0.15.0

The package source is embedded as `const &str` templates in `tyc/src/commands/build.rs` (`TYPHON_RUNTIME_INIT_PY`, `TASKS_PY`, `LAZY_PY`, `STDLIB_PY`, `RESULT_PY`, `PARALLEL_PY`, `FREEZE_PY`, `CAST_PY`, `TRACEBACK_PY`).

### 1.1 Package layout

| File | Surface |
|---|---|
| `__init__.py` | Re-exports `Ok`, `Err`, `Result` so `from typhon_runtime import Ok, Err, Result` works |
| `result.py` | `Ok[T]`, `Err[E]`, `Result[T, E]` dataclasses + `.map`/`.map_err`/`.and_then`/`.or_else` methods (v0.6.0). Frozen, slots-based. CPython repr matches the v0.3.0 O24 fix (`Ok(value=20)` / `Err(error='oops')`). Also exports **`try_result(thunk, on_err=None)`** (v0.15.0): runs `thunk()` → `Ok(result)`; on any exception → `Err(on_err(exc))`, or `Err(exc)` when no mapper is given. `tyc build` auto-injects `from typhon_runtime import try_result`; it's a checker prelude name (typed `Result[T, E]`) and a VM prelude native that materialises the caught exception as an `except E as e:` handler would. |
| `tasks.py` | `spawn(coro)` — adds the task to a strong-ref `_BACKGROUND` set with a done-callback `discard` to prevent GC mid-flight. Closes the asyncio `create_task` weakref gotcha. |
| `lazy.py` | `_LazyModule` / `lazy_import(name)` — attribute-proxy with double-checked locking. `_LazyValue` / `lazy_let(factory)` — sentinel-cached single-shot evaluator. |
| `parallel.py` | `map_pure(fn, iterable)` — thread-pool-backed parallel map (`concurrent.futures.ThreadPoolExecutor` by default; degrades to sequential on GIL-locked CPython). `[strictness] parallel-backend = "interpreters"` bakes in a PEP 734 `InterpreterPoolExecutor` (3.14+) attempt first, with transparent fallback to the thread pool on an older runtime or an unshareable mapped callable. Used by list/set/dict comprehension rewrites under `auto-parallel`, and by integer accumulator-loop reductions under `auto-parallel-reductions`. |
| `freeze.py` | `deep_freeze(obj)` — recursively replaces `list → tuple`, `dict → MappingProxyType`, `set → frozenset`; raises `TypeError` at startup on un-freezable values (file handles, sockets, generators, non-frozen dataclasses). |
| `cast.py` (v0.14.0) | `checked_cast(value, tp)` — backs `EXPR as! TYPE`. Recursively verifies `value` against `tp` via `typing.get_origin`/`get_args` (scalars, `list`/`set`/`frozenset`/`dict`/`tuple`, unions/`Optional`), honouring `int → float` widening; raises `TypeError` on a mismatch, returns the value on success. Targets it can't model (`Any`/`object`/TypeVars) pass through. |
| `traceback.py` (v0.14.0) | `install()` — sets a `sys.excepthook` that loads the running script's `.sourcemaps/*.py.map`, then text-rewrites each `File "…​.py", line N` frame to the `.ty` location. Emitted only when `[emit] traceback-remap = true`; the entry module's `__main__` block calls `install()`. |
| `stdlib.py` | Internal helpers used by lowering passes |

### 1.2 User-facing contract

**The runtime is generated; users do not edit it and do not pip-install it.** It is regenerated on every `tyc build`. There is no PyPI package — every project ships its own copy alongside the emitted `.py`. Anyone consuming the emitted code from outside Typhon just imports `typhon_runtime` like any other package in the project's `src` / `build` tree.

Users should:

- **Treat `typhon_runtime/` as a build artifact.** Don't add it to source control — let `tyc build` regenerate it.
- **Use `from typhon_runtime import Ok, Err, Result` in `.dty` stubs** if the stub describes Typhon-aware code.
- **Never reach into `typhon_runtime.tasks._BACKGROUND` etc.** — implementation detail, may change.

### 1.3 `Ok` / `Err` / `Result` shape

```python
@dataclass(slots=True, frozen=True)
class Ok(Generic[T]):
    value: T

    def map[U](self, f: Callable[[T], U]) -> "Result[U, Any]":
        return Ok(f(self.value))

    def map_err[F](self, f: Callable[[Any], F]) -> "Ok[T]":
        return self

    def and_then[U, E](self, f: Callable[[T], "Result[U, E]"]) -> "Result[U, E]":
        return f(self.value)

    def or_else[U, E](self, f: Callable[[Any], "Result[U, E]"]) -> "Ok[T]":
        return self


@dataclass(slots=True, frozen=True)
class Err(Generic[E]):
    error: E

    def map[U](self, f: Callable[[Any], U]) -> "Err[E]":
        return self

    def map_err[F](self, f: Callable[[E], F]) -> "Err[F]":
        return Err(f(self.error))

    def and_then[T, F](self, f: Callable[[Any], "Result[T, F]"]) -> "Err[E]":
        return self

    def or_else[T, F](self, f: Callable[[E], "Result[T, F]"]) -> "Result[T, F]":
        return f(self.error)


Result = Ok[T] | Err[E]
```

The exact emission may differ slightly; the contract is: `Ok.value`, `Err.error`, the four combinators, frozen, slot-based, hashable.

### 1.4 `tasks.spawn` shape

```python
_BACKGROUND: set[asyncio.Task] = set()

def spawn(coro: Coroutine[Any, Any, T]) -> asyncio.Task[T]:
    task = asyncio.create_task(coro)
    _BACKGROUND.add(task)
    task.add_done_callback(_BACKGROUND.discard)
    return task
```

The strong-ref registry plus the done-callback closes the well-known asyncio gotcha (event loop holds only weak refs to tasks; a fire-and-forget task whose handle is dropped can be GC'd mid-flight).

### 1.5 `lazy_let` shape

```python
_SENTINEL = object()

class _LazyValue(Generic[T]):
    __slots__ = ("_factory", "_value", "_lock")

    def __init__(self, factory: Callable[[], T]) -> None:
        self._factory = factory
        self._value: Any = _SENTINEL
        self._lock = threading.Lock()

    def __call__(self) -> T:
        if self._value is _SENTINEL:
            with self._lock:
                if self._value is _SENTINEL:
                    self._value = self._factory()
        return self._value  # type: ignore[return-value]


def lazy_let(factory: Callable[[], T]) -> Callable[[], T]:
    return _LazyValue(factory)
```

Thread-safe one-shot. Module-level `lazy let CFG: Config = load()` lowers to `CFG = lazy_let(lambda: load())`; the first access pays the load cost, subsequent accesses are memory reads.

### 1.6 `deep_freeze` shape

```python
def deep_freeze(value: Any) -> Any:
    if isinstance(value, (int, float, complex, str, bytes, bool, type(None))):
        return value
    if isinstance(value, tuple):
        return tuple(deep_freeze(v) for v in value)
    if isinstance(value, list):
        return tuple(deep_freeze(v) for v in value)
    if isinstance(value, frozenset):
        return frozenset(deep_freeze(v) for v in value)
    if isinstance(value, set):
        return frozenset(deep_freeze(v) for v in value)
    if isinstance(value, dict):
        return types.MappingProxyType({
            deep_freeze(k): deep_freeze(v)
            for k, v in value.items()
        })
    if dataclasses.is_dataclass(value) and getattr(type(value), "__dataclass_params__", None) is not None:
        params = type(value).__dataclass_params__
        if getattr(params, "frozen", False):
            return value
    raise TypeError(f"deep_freeze: value of type {type(value).__name__} is not freezable")
```

Frozen dataclasses pass through unchanged. File handles, sockets, generators, non-frozen dataclasses, and anything else without a clean immutable equivalent raise `TypeError` at startup so the failure is loud.

---

## Part 2 — The in-process VM (`tyc-vm`)

`tyc-vm` is a Rust tree-walking interpreter that runs `.ty` source **without ever emitting Python**. It is the **default** mode for `tyc run`. There is no `build/`, no `typhon_runtime.py`, no CPython spawn.

### 2.1 Architecture

```
.ty source
  → tyc-syntax::preprocess (expand ?, |>, gather:, go, with-chains, lazy)
  → tyc-syntax::parse_module
  → tyc-vm::Interpreter
      ├── env.rs       lexical scope chains with global/nonlocal
      ├── value.rs     Value enum (Rc-based), Python-compatible eq/ord/hash
      ├── interp.rs    statement + expression evaluators
      ├── builtins.rs  native fns, method dispatch, stdlib modules
      └── ffi.rs       file-handle shim (no PyO3 yet)
```

The VM's `Value` enum covers `Int(BigInt)` (v0.8.0 switched from `i64`; programs that relied on silent wrap-around compute different (correct) results), `Float`, `Bool`, `Str`, `Bytes`, `None`, `List`, `Tuple`, `Dict` (insertion-ordered `IndexMap` since v0.8.0; `del d[k]` / `dict.pop` use `shift_remove`), `Set`, `FrozenSet`, `Function`, `Method`, `Class`, `Instance`, `NativeFunction`, `Module`, `Iterator`, `Generator`, `Coroutine`, `Task`, `Ok`/`Err` (with bound `.map`/`.map_err`/`.and_then`/`.or_else` combinators since v0.9.0). Reference counting via `Rc<RefCell<...>>` — no GC, no thread sharing. Recursion limit is 1000 frames (was 256 before v0.8.0) to match CPython's default.

### 2.2 Why a VM at all?

- **Snappy iteration loop.** No build step, no `typhon_runtime/` materialisation, no CPython spawn. `tyc run hello.ty` is sub-100ms cold-start for typical hello-world programs.
- **Build-free CI.** `tyc check && tyc run -- some-test-input` exercises the program without emitting Python.
- **Education / REPL.** The VM backs `tyc repl` (in compile-then-exec mode today, but designed for future direct integration).

### 2.3 Supported language surface

All standard literals, including f-strings with `{x:.2f}` width / precision / commas. `let`, `mut`, walrus `:=`, augmented assigns, tuple/list unpacking with starred targets.

Full `if`/`elif`/`else`/`while`/`for`/`break`/`continue`/`return`/`pass`. `match` with literal / capture / wildcard / sequence / class patterns, including native `Ok(x)` / `Err(e)` matching. Arm bodies execute against the parent env (v0.3.1 N9 fix — pattern captures lift into the parent env on accept, matching CPython).

Functions with positional/keyword/default/`*args`/`**kwargs`/closures, recursion (a 1000-frame limit by default, matching CPython's `sys.getrecursionlimit()`; `sys.setrecursionlimit(n)` moves it and rejects `n < 1` with CPython's own `ValueError`; exceeding it raises `RecursionError`). Classes with annotated-field constructor synth, explicit `__init__`, methods (in `class` body or sibling `impl Foo:`), single and multi-level inheritance (fields accumulate across the full MRO, v0.11.0). `enum Name:` declarations resolve natively against the VM's `enum` shim (v0.11.0).

Decorators: `@pure`, `@dataclass`, `@gatherable`, `@override`, `@final`, `@staticmethod` (no-ops); `@memo` / `@cache` / `@lru_cache` (value-keyed memoisation). **`@property` getters fire on attribute read and `@classmethod` binds `cls` (v0.10.0)** — both inherited through bases, with the descriptor marker cleared on override.

**Dunder dispatch on user instances (v0.10.0 / v0.11.0):** every numeric / bitwise / matmul operator and its reflected form (`__add__` / `__radd__` / … / `__matmul__`), the rich comparisons (`__eq__` / `__ne__` / `__lt__` / `__le__` / `__gt__` / `__ge__`), and `__str__` / `__repr__` / `__len__` / `__getitem__` / `__setitem__` / `__contains__` / `__call__` / `__missing__` / `__post_init__`. `in`, `list.index`, `.count`, `.remove`, and `sorted` / `min` / `max` / `list.sort` use `__eq__` / `__lt__`-aware comparison (the last three fixed in v0.12.0). Bare `super()` is rewritten to the two-arg form at desugar so it works under `@dataclass(slots=True)`.

**`type(x)` is a real type object (v0.10.0):** `type(x).__name__`, `type(x) == int`, `str(type(x))` → `<class 'int'>` all work.

**Generators (v0.10.0):** `yield` / `yield from` work, materialised **eagerly** — a yield-bearing function runs to completion with each yielded value collected, and the call returns an iterator over them (a tree-walk can't suspend a frame; `Rc` values aren't `Send`). `GENERATOR_CAP = 1_000_000` bounds the `while True: yield` worst case to a clear `RuntimeError`. **Lazy / unbounded generators, and `@contextmanager` generators used inside a `with` block, still need `tyc build`** (eager collection runs setup + teardown at call time).

`try`/`except T as e`/`else`/`finally`/`raise` (exception-type matching walks the MRO). `Result` ADT with native `Ok(...)` / `Err(...)`, the `.map` / `.map_err` / `.and_then` / `.or_else` combinators (v0.9.0), and `?`. Imports of native stdlib (table below). Comprehensions (eagerly materialised); dict comprehensions bind tuple-unpack targets (v0.12.0).

A broad built-in surface:
`print`, `len`, `range`, `str`, `int` (incl. `int(s, base)` / `base=0`), `float`, `bool`, `complex` (v0.11.0), `list`, `tuple`, `dict`, `set`, `frozenset`, `repr`, `ascii`, `format`, `type`, `isinstance`, `abs`, `min`, `max`, `sum`, `sorted`, `reversed`, `enumerate`, `zip`, `map`, `filter`, `all`, `any`, `next`, `iter`, `divmod`, `pow` (2- and 3-arg), `hex`, `bin`, `oct`, `chr`, `ord`, `round` (banker's rounding, v0.11.0), `input`, `hash`, `id`, `callable`, `open` (read / write / append / binary modes).

Built-in method dispatch on `str` / `bytes` (v0.11.0) / `list` / `dict` (incl. `popitem` / `fromkeys`, v0.12.0) / `set` (full algebra) / `tuple` / `int.bit_length` / `float.is_integer`. `dict.keys()` / `.values()` / `.items()` return real view objects (v0.11.0). Set operators `|` / `&` / `-` / `^`. VM value semantics match CPython since v0.11.0 (value-based dataclass equality keyed on class identity, `Name(field=value)` repr, hashable instances, order-independent set equality, shortest-round-trip float repr).

### 2.4 Native stdlib modules

| Module | Surface |
|---|---|
| `math` | `pi`, `e`, `inf`, `nan`, `sqrt`, `floor`, `ceil`, `log` (with base), `log2`, `log10`, `exp`, `sin`, `cos`, `tan`, `pow`, `fabs`. v0.10.0: `gcd`, `lcm`, `factorial`, `isqrt`, `comb`, `perm` (reject non-int args). v0.12.0: `isnan`, `isinf`, `isfinite` |
| `os` / `os.path` (rebuilt v1.0.0-beta.1) | Full process + filesystem surface over native `_fs_*` primitives: `getenv`, `environ`, `getcwd`, `chdir`, `listdir`, `scandir`, `walk`, `mkdir`, `makedirs`, `remove`, `rmdir`, `rename`, `replace`, `stat`, `access`, `getpid`, `cpu_count`, `system`, `strerror`, `urandom`, the `O_*` / `*_OK` / `SEEK_*` constants, `PathLike` / `fspath`, and a full `posixpath` (`join`, `split`, `splitext`, `basename`, `dirname`, `normpath`, `abspath`, `realpath`, `relpath`, `commonpath`, `commonprefix`, `expanduser`, `expandvars`, `isabs`, `exists`, `isfile`, `isdir`, `islink`, `getsize`, `getmtime`, `samefile`). Errors carry CPython's `errno` / `strerror` / `filename`. `import os.path` / `import posixpath` resolve to the same shim |
| `sys` | `argv`, `platform`, `version`, `version_info`, `byteorder`, `maxsize`, `exit(code)`, `stdout`, `stderr`, `stdin`, `getrecursionlimit` / `setrecursionlimit`. v1.0.0-beta.1: `modules` (live view of the import cache) and `exc_info()` (traceback slot is always `None`). Assigning `sys.stdout` redirects `print`, so `contextlib.redirect_stdout` works |
| `json` | `dumps`, `loads` (full JSON 7159 surface). `json.load(f)` / `json.dump(obj, f)` ride on top of `open()` since v0.9.0. v0.10.0: `dumps(indent=…)`. v0.12.0: `dumps(sort_keys=True)` |
| `time` | `time()`, `sleep()`, `monotonic()` (fixed in v0.10.0), `perf_counter()` / `process_time()` (v0.10.0) |
| `datetime` (v0.11.0) | `datetime(y, mo, d, …)`, `.now()`, `.fromisoformat()`, `.isoformat()`, `+ timedelta`, comparisons; `timedelta(seconds=…)` arithmetic. **Naïve / UTC only** — tz-aware arithmetic still needs `--compile` |
| `enum` (v0.11.0) | `enum.Enum`, `enum.auto()` — backs the `enum` keyword; members materialise in declaration order, `<Name.MEMBER: val>` repr |
| `random` | `random()`, `seed(n)`, `getrandbits`, `randint`, `randrange`, `uniform`, `gauss`, `choice`, `shuffle`, `sample` — a **CPython-compatible MT19937** (follows `random.py` / `_randommodule.c`), so `random.seed(n)` yields a **byte-identical sequence** under `tyc run` and `tyc build` + CPython. Unseeded, it seeds from OS-derived entropy like CPython does at import, so each run differs (v1.0.0-alpha.8; previously a fixed constant, which made `tyc run` repeatable where CPython is not). String / bytes / float seeds are rejected — use `tyc run --compile`. **Not** cryptographic: use `secrets` |
| `re` (v0.8.0) | `match`, `search`, `findall`, `sub`, `split`, `compile`. `match` is anchored at the start of the string. Some flag arguments accepted but ignored |
| `typing` (v0.8.0) | Generic constructors are runtime no-ops; type-only imports are stripped by the desugar pre-pass |
| `collections.abc` | The abstract container types (`Callable`, `Iterable`, `Iterator`, `Generator`, `Sequence`, `Mapping`, `MutableMapping`, `Set`, `Hashable`, `Awaitable`, `Coroutine`, `AsyncIterator`, the `*View`s, …) as identity natives — annotation-only at runtime, like the `typing` shim |
| `abc` (v0.15.6) | `ABC`, `ABCMeta`, `abstractmethod`, `abstractclassmethod`, `abstractstaticmethod`, `abstractproperty`, `update_abstractmethods` as identity natives; a non-class base such as `ABC` is ignored at class creation, so `class H(ABC): @abstractmethod def handle(...)` runs |
| `asyncio` | Cooperative-sequential shim: `run`, `gather` (incl. `return_exceptions=True`), `TaskGroup` (`create_task`, `__aenter__` / `__aexit__`), `sleep` (real wall-clock), `timeout` (checked at scope exit), `Queue` (fails loudly instead of deadlocking), and the exception classes (`TimeoutError`, `CancelledError`, …). Coroutines are forced to completion at their `await` — see §2.5 for the interleaving caveat |
| `collections` (v0.8.0) | `OrderedDict`, `defaultdict`, `Counter`, `namedtuple`. **`collections.deque`** added in v0.9.0 — rides on `Value::List` via new `popleft` / `appendleft` / `extendleft` / `rotate` list methods. v0.11.0: `defaultdict(factory)` actually invokes the factory on missing-key access via subscript `__missing__` (`dd[k] += 1` works) |
| `functools` (v0.8.0) | `lru_cache`, `cache`, `cached_property`, `reduce`, `partial`, `wraps`. v1.0.0-beta.1: `partial` binds keyword arguments (`partial(pow, exp=2)`), plus `cmp_to_key`, `total_ordering`, `singledispatch` |
| `itertools` (v0.8.0) | `chain`, `count`, `cycle`, `accumulate`, `combinations`, `permutations`, `product`, `islice`, `takewhile`, `dropwhile`, `groupby` (honours `key=` since v0.11.0) |
| `dataclasses` (v0.8.0) | `dataclass`, `field`, `fields`, `asdict`, `astuple`. v0.9.0: `field(default_factory=list)` actually invokes the factory per instance |
| `pathlib` (v0.8.0, rebuilt v1.0.0-beta.1) | `PurePath` / `Path` over the `os` shim: normalisation, `parts`, `parent(s)`, `name`, `stem`, `suffix(es)`, `with_name` / `with_stem` / `with_suffix`, `joinpath` / `/`, `relative_to`, `is_absolute`, `match`, `as_posix`, `as_uri`, comparison + hashing, `home` / `cwd` / `absolute` / `resolve` / `expanduser`, `exists` / `is_file` / `is_dir` / `stat`, `read_text` / `write_text` / `read_bytes` / `write_bytes` / `open`, `iterdir` / `glob` / `rglob` / `walk`, `mkdir` / `touch` / `unlink` / `rmdir` / `rename`. `repr` and error messages match CPython |
| `heapq` (v0.9.0) | `heappush`, `heappop`, `heapify`, `heappushpop`, `heapreplace`, `nsmallest`, `nlargest` |
| `contextlib` (v0.9.0) | `@contextmanager` identity decorator; `with` block honours the wrapped `__enter__` / `__exit__` shape. v1.0.0-beta.1: `suppress`, `nullcontext`, `closing`, `redirect_stdout`, `redirect_stderr`, `ExitStack` |
| `pydantic` (v0.9.0) | `BaseModel` placeholder so declaring a `model` doesn't `ImportError` (full validation still requires `--compile`) |
| `io` (v1.0.0-beta.1) | `open` and its file objects (`TextIOWrapper`, `BufferedReader` / `BufferedWriter`, `FileIO`), `StringIO`, `BytesIO`, `SEEK_*`, `UnsupportedOperation`. Modes, encodings, newline translation, `seek` / `tell` / `truncate` / `flush`, line iteration, CPython error messages |
| `shutil` (v1.0.0-beta.1) | `copy`, `copy2`, `copyfile`, `copytree`, `move`, `rmtree`, `which`, `disk_usage`, `SameFileError` |
| `tempfile` (v1.0.0-beta.1) | `mkdtemp`, `mkstemp`, `gettempdir`, `NamedTemporaryFile`, `TemporaryDirectory`, `TemporaryFile` |
| `glob` (v1.0.0-beta.1) | `glob`, `iglob`, `escape`, `has_magic` (incl. `**`) |
| `hashlib` (v1.0.0-beta.1) | `md5`, `sha1`, `sha224`, `sha256`, `sha384`, `sha512`, `blake2b`, `blake2s`, `new` + `update` / `digest` / `hexdigest` / `copy` |
| `base64` (v1.0.0-beta.1) | `b64encode` / `b64decode` (incl. `altchars`), `urlsafe_*`, `standard_*`, `b32*`, `b16*`, `encodebytes` / `decodebytes` |
| `csv` (v1.0.0-beta.1) | `reader`, `writer`, `DictReader`, `DictWriter`, `QUOTE_*`, `excel` / `excel-tab` / `unix` dialects, `register_dialect`. No `Sniffer`, no `escapechar` |
| `string` (v1.0.0-beta.1) | Constant tables, `capwords`, `Template` (`substitute` / `safe_substitute`) |
| `operator` (v1.0.0-beta.1) | Operator function forms plus `itemgetter`, `attrgetter`, `methodcaller`, `countOf`, `indexOf`, `length_hint` |
| `bisect` (v1.0.0-beta.1) | `bisect_left` / `bisect_right` / `insort_left` / `insort_right` (+ aliases), with `lo` / `hi` / `key` |
| `__future__` (v1.0.0-beta.1) | The feature flags, so `from __future__ import annotations` imports rather than raising |
| `typhon_runtime` (and `typhon_runtime.*`) | `Ok`, `Err`, `tasks.spawn` (synchronous shim), `lazy.lazy_let`, `lazy.lazy_import`. `Ok` / `Err` carry bound `.map` / `.map_err` / `.and_then` / `.or_else` combinators since v0.9.0. Submodule imports (`from typhon_runtime.freeze import deep_freeze`) resolve to the matching submodule |

`enum` is resolved separately by the interpreter (it backs the `enum` keyword), not through this table.

**Anything outside this set takes the compiled path automatically**
(v1.0.0-beta.1): `tyc run` scans the program's imports before executing
anything, prints a `note:` naming the unmodelled module, and runs the
program through `tyc build` + CPython. `--no-fallback` refuses instead.

Deliberately **not** modelled (they need a real CPython runtime, not a
shim): `sqlite3`, `subprocess`, `threading` / `multiprocessing`, `socket`
/ `urllib` / `http`, `ctypes`, `decimal`, `fractions`, `logging`,
`configparser`, `struct`, and every third-party package.

Also new in v1.0.0-beta.1: `issubclass`; `str.isascii` / `isidentifier` /
`isprintable`; the `numbers` tower on `int` / `float` (`real`, `imag`,
`conjugate()`, `numerator`, `denominator`) and `int.from_bytes`; the
unbound method form of every builtin type (`str.upper(s)`,
`dict.get(d, k)`); function objects carrying their own `__dict__`; class
objects hashable by identity (type-keyed registries); `class X(NamedTuple)`
being a real tuple and `class X(TypedDict)` constructing a plain `dict`.

### 2.4a Built-in builtins surface (v0.9.0 → v0.12.0 additions)

- **`open(p, mode)`** honours `"r"` / `"w"` / `"a"` / `"wb"` / `"r+"` and friends, plus `__enter__` / `__exit__` for `with` blocks. Text and binary modes both work (v0.9.0). `read_text` / `write_text` / `open` tolerate (and ignore) `encoding=` / `errors=` — the VM is always UTF-8 (v0.10.0).
- **`frozenset(...)`** is hashable as a dict key via `HashKey::FrozenSet` with insertion-order-independent hashing (v0.9.0).
- **`@property` getters fire on read, `@classmethod` binds `cls`** (v0.10.0) — both inherited, marker cleared on override. `@staticmethod` is a no-op; bare `super()` is rewritten to the two-arg form so it works under `@dataclass(slots=True)` (v0.11.0).
- **f-string `_` thousands separator** (v0.9.0), **`{x=}` debug conversion** (v0.11.0), and full format-flag parity (zero-pad / align / sign / width / comma / precision / type, v0.8.0; float-presentation types coerce int/bool operands, v0.12.0).
- **`bytes` repr matches CPython** byte-for-byte (`b'hi'` by default, `b"with 'embedded'"` fallback, `\xNN` for non-printable, v0.9.0); `bytes` methods `decode` / `hex` / `fromhex` / `count` / `find` / `split` / `strip` / … (v0.11.0).
- **Class patterns on built-in types** in `match` — `case str() as s:`, `case int() as n:`, mapping patterns (`case {"k": v}`), sequence-with-star (`case [x, *rest, y]`) (v0.8.0–v0.9.0).
- **v0.10.0 builtins:** `divmod`, `pow` (2- and 3-arg modular), `format`, `ascii`, `int(s, base)` (incl. `base=0`), full set algebra (`union` / `intersection` / `difference` / `symmetric_difference` / `issubset` / `issuperset` / `isdisjoint` / `update`), the missing string methods (`center` / `ljust` / `rjust` / `zfill` / `rsplit` / `partition` / `rpartition` / `removeprefix` / `removesuffix` / `casefold` / `expandtabs` / …, and `strip` / `lstrip` / `rstrip` honour their `chars` arg), `dict(other)` / `dict(**kwargs)`. `max` / `min` / `list.sort` accept `key=` / `reverse=` / `default=` via a kwargs sentinel.
- **v0.11.0:** `complex(...)` and complex literals (`Value::Complex`), `dict.keys()` / `.values()` / `.items()` view objects (`Value::DictView`), banker's-rounding `round`, `str %` / f-string `%` runtime formatting, real `re.Match.group(n)` / `.groups()` / `.groupdict()`, `str.split(maxsplit=)`.
- **v0.12.0:** `dict.popitem()`, `dict.fromkeys(iterable[, value])`, `str.maketrans(...)`, `str.translate(table)`, `str.replace(old, new, count)` (honours `count`), stable `sorted(reverse=True)` / `list.sort(reverse=True)`, and `sorted` / `min` / `max` honouring a user `__lt__`.

### 2.4b Multi-file projects (v0.9.0)

The VM walks the project source root and loads sibling `.ty` modules on demand. Relative imports (`from .repo import x`, `from ..pkg.users import load`) resolve through `Value::Module` cache so circular dependencies are fine. `tyc run --compile` spawns `python -m <pkg>.main` instead of `python build/main.py` so relative imports in the entry point resolve correctly under the compiled path too.

### 2.4c Freeze / comptime parity with `tyc build` (v0.9.0)

`freeze let CFG = {...}` recursively wraps the value (list → tuple, dict → mappingproxy-tagged dict, set → frozenset) so mutations through aliased references raise the same `TypeError` CPython's `MappingProxy` does. `comptime let X = ...` inlines via the substitution pass shared with `tyc build`, so `comptime let PORT = int(env(...))` no longer crashes with `NameError: env is not defined` under `tyc run`.

`EXPR as! TYPE` (v0.14.0) runs the **same recursive structural check** under the VM as on the compiled path. The VM intercepts the direct `__typhon_checked_cast__(value, TYPE)` call, interprets the type-descriptor AST (scalars with `int→float`/`bool→int` widening, `list[X]`/`set[X]`/`frozenset[X]`, `dict[K, V]`, fixed- and variadic-`tuple[...]`, `Optional`/unions, user classes via `isinstance`), and raises `TypeError` on a shape mismatch — so a deliberately-wrong `as!` is rejected under `tyc run` exactly as it is under `tyc build && python build/main.py`. (Only the rare *indirect* path — `checked_cast` referenced as a value rather than called directly with two args — degrades to identity, because the type descriptor isn't available as an AST there; the direct call the lowering always emits takes the checked path.)

### 2.5 Not supported (surfaces as `NotImplementedError` or `ImportError`)

- **Real (interleaved) async concurrency** — `async def` / `await` / `gather:` / `go` / `asyncio.run` / `asyncio.gather` / `TaskGroup` **do run** under the VM via a **cooperative-sequential** scheduler: calling an `async def` produces a coroutine thunk (the body doesn't run yet, matching CPython), and `await` / `asyncio.run` / `gather:` / `spawn` force it to completion in program order. Results match CPython whenever correctness doesn't depend on task *interleaving*; programs that genuinely depend on interleaving (e.g. a producer/consumer handing off through an `asyncio.Queue` mid-flight) fail **loudly** with a `RuntimeError` pointing at `tyc run --compile` rather than deadlocking. (Earlier releases surfaced a `NotImplementedError` for any `async def`; the cooperative scheduler superseded that.) See `docs/vm.md` for the full async surface.
- **Lazy / unbounded generators** — `yield` / `yield from` *do* work since v0.10.0, but **eagerly**: a yield-bearing function runs to completion and returns an iterator over the collected values, capped at `GENERATOR_CAP = 1_000_000` (a `while True: yield` raises a clear `RuntimeError`). Genuinely infinite or lazily-consumed generators need `tyc build`.
- **`@contextmanager` generators used inside a `with` block** — the eager-collection model runs setup + teardown at call time, so the `with` body can't run between them. The decorator is recognised (identity) and `@contextmanager` *factory bodies* are exempt from `resource_not_managed`, but a generator-based context manager driven by a `with` needs `tyc build`.
- **Nested-model `pydantic.model_validate`** — flat `model` classes work under the VM (v0.10.0: `model_validate` / `model_dump` / `model_dump_json`), but nested-model validation is not type-directed; deeply-nested models need `tyc build`.
- **Tz-aware `datetime` arithmetic** — the native `datetime` shim (v0.11.0) is naïve / UTC only; tz-aware arithmetic needs `tyc build`.
- **Template strings** (`t"…"`).
- **IPython escapes** (`%foo`, `!bar`, etc.).
- **`with` statements other than `open()` / `@contextmanager`-decorated factories** — basic context-manager protocol covers the common case since v0.9.0; arbitrary user `__enter__` / `__exit__` on plain classes is a follow-up.
- **Most third-party packages** — `numpy`, `requests`, `pandas`, full `pydantic` validation, etc. (placeholders only).

### 2.6 Fallback rule

`tyc run` does **not** fall back to the compiled path on its own: when the program imports a module the VM can't speak natively, the VM raises `ImportError` with a pointer to `--compile` (alias `--no-vm`), and you re-run with that flag.

As of **v0.3.1**, `tyc run` (VM mode) gates on the static `tyc check` pipeline first — unresolved names, type mismatches, and arity errors fail the same way `tyc check` would. Set `TYC_SKIP_CHECK=1` to bypass for stress harnesses; `--compile` has no equivalent bypass because the build pipeline always type-checks.

```bash
tyc run                    # default — VM
tyc run --compile          # build-then-exec via CPython
tyc run --compile --temp   # build into a tempdir deleted on exit
TYC_SKIP_CHECK=1 tyc run   # bypass pre-VM check
```

### 2.7 Performance and roadmap

The VM trades steady-state compute throughput for startup latency and CPython parity, not the other way round. On a hello-world program it still wins on startup (~21ms vs ~38ms for `tyc build` + a CPython 3.13 process spawn). At steady-state compute the tree-walker is roughly **3–14× slower startup-adjusted (~2.7–6× end-to-end wall clock)** against `tyc build` + CPython 3.13, down from the ~5–18× (startup-adjusted) measured before this branch's Tier 1 representation/dispatch work (small-int `i64` fast path with overflow promotion to `Rc<BigInt>`, a per-class method-resolution cache, direct method-call dispatch, and slot-resolved locals for eligible functions — arbitrary-precision int semantics are preserved exactly throughout). Recursive, call-heavy integer arithmetic remains the worst case; object-construction- and method-call-heavy code fares best. See `docs/vm.md`'s Performance section and `docs/vm-performance-plan.md` for the full measured baseline and the tiered plan. Tier 2 — a bytecode VM, the point where rough CPython parity on most code becomes realistic — is designed but not yet started. PyO3-backed FFI for unsupported modules is an explicit **non-goal** for now (it would reintroduce a CPython dependency into what is currently a pure-Rust execution path).

### 2.8 Diagnostics

Runtime failures surface as:

```text
Traceback (most recent call last):
KIND: MESSAGE
```

The traceback names frames but not source lines (full source-line tracebacks inside the VM are a tracked follow-up). For full fidelity today, use `tyc run --compile` then `tyc trace` on the captured traceback — that path is fully `.py.map`-aware.

### 2.9 When to use which mode

| Situation | Use |
|---|---|
| Quick iteration on pure-Typhon code (no numpy / requests / etc.) | `tyc run` (default VM) |
| Need a third-party library | `tyc run --compile` |
| Production deployment | `tyc build` then run `python build/main.py` as usual |
| CI smoke test for compile correctness | `tyc check && tyc build --check` |
| Stepping through emitted code | `tyc debug` (Typhon-aware pdb wrapper) |
| Capturing a Python traceback and mapping back to `.ty` | `python build/main.py 2> err.log && tyc trace err.log` |

---

## Part 3 — `.py.map` v2 source maps

Every emitted `.py` ships with a `<out>/.sourcemaps/<rel>.py.map` sidecar (v0.6.1 — legacy adjacent `<out>/<rel>.py.map` layout still readable). The map is **v2**: a per-statement `(out_line → ty_line)` table. Format is JSON.

Consumers:

- **`tyc trace`** — reads a captured Python traceback from stdin or a file and rewrites every `path.py:LINE[:COL]` reference back to `.ty`. Paths with spaces use a longest-candidate walk-left lookup (v0.5.0). Cached per-line.
- **`tyc debug --break <ty-file>:<line>`** — translates the Typhon source location through `.py.map` and injects `-c "break build/main.py:N"` into the debugger session.
- **`tyc debug` source-mapping wrapper** (v0.5.0) — loads every `.py.map` at startup, overrides `pdb.Pdb`'s `do_list`, `do_where`, `format_stack_entry`, and `prompt`. The entire debugger UI reads `.ty` paths and source slices. `--raw-pdb` opts out.
- **`tyc ty`** (v0.5.0) — same loader as `tyc trace` (shared `commands/source_map.rs`). Rewrites `ty`'s `*.py:LINE[:COL]` diagnostics to `.ty` coordinates. `--raw` opts out.
- **`tyc lsp`** — cross-file go-to-definition crosses the `.ty` / `.py` boundary via the resolver's `resolved_module` query plus `.py.map`.

Lines without a recognisable `.py:` reference (summary text, blank lines, snippet excerpts) are forwarded unchanged.

---

## Part 4 — `.dty` stubs and `.pyi` emission

`.dty` is Typhon's stub format. Bodies omitted; signatures + class/method declarations + annotated fields only. Every `.dty` next to the project compiles to a PEP 561 `.pyi` during `tyc build` — always on (the previously-documented `[emit] pyi-stubs` toggle has been removed).

Workflow:

1. Author `foo.dty` with declarations only.
2. `tyc build` emits `foo.pyi` so mypy / pyright / Pyrefly / `ty` see the Typhon surface API.
3. `tyc check --stubs` parses every `.dty` and diffs its surface against the `.ty`/`.py` implementation — missing-in-impl, missing-in-stub, signature-mismatch → `tyc::stub_mismatch`. Severity controlled by `[strictness] stub-check`.
4. `tyc stubtest` runs `python -m mypy.stubtest` against every emitted `.pyi`. Catches dynamically-created members the AST can't see (`__init_subclass__` injection, metaclass-driven member registration, Pydantic auto-generated fields).

**Influence on the checker.** Cross-module shape extraction consumes both `.ty` and `.dty` on equal footing. **When both define the same name, stubs win.** This makes `.dty` the natural way to describe foreign Python libraries to the Typhon checker.

Example `.dty`:

```python
# src/stubs/redis.dty
class Redis:
    host: str
    port: int

impl Redis:
    def get(self, key: str) -> str?
    def set(self, key: str, value: str) -> bool
    def delete(self, *keys: str) -> int
    def keys(self, pattern: str = "*") -> list[str]
```

---

## Part 5 — Install paths and binary distribution

Pre-built binaries ship on every GitHub Release.

| OS | Arch | Target triple | Archive |
|---|---|---|---|
| macOS | Apple Silicon (`arm64`) | `aarch64-apple-darwin` | `.tar.gz` |
| macOS | Intel (`x86_64`) | `x86_64-apple-darwin` | `.tar.gz` |
| Linux | `x86_64` | `x86_64-unknown-linux-gnu` | `.tar.gz` |
| Linux | `aarch64` | `aarch64-unknown-linux-gnu` | `.tar.gz` |
| Windows | `x86_64` | `x86_64-pc-windows-msvc` | `.zip` |

Linux artifacts are built on Ubuntu 22.04 / glibc 2.35. Ubuntu 22.04+, Debian 12+, Fedora 36+, RHEL 9+, Arch all supported. Alpine/MUSL: install `gcompat` or build from source.

### macOS / Linux

```bash
curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh
```

The script detects OS+arch, resolves the latest tag via the GitHub API, downloads the tarball + `SHA256SUMS`, verifies with `shasum -a 256 -c` or `sha256sum -c`, extracts, installs `tyc` to `$HOME/.local/bin` (no `sudo`). On macOS it clears `com.apple.quarantine` so Gatekeeper doesn't prompt. Re-running upgrades in place.

Env vars / flags: `TYPHON_VERSION=v0.12.0`, `TYPHON_INSTALL_DIR=/opt/typhon/bin`, or `sh install.sh --version=v0.12.0 --dir=/opt/typhon/bin`.

### Windows (PowerShell)

```powershell
iwr -useb https://raw.githubusercontent.com/codehalwell/typhon/main/install.ps1 | iex
```

Detects arch, downloads zip + `SHA256SUMS`, verifies with `Get-FileHash`, extracts to `%LOCALAPPDATA%\Programs\Typhon\` (no admin), adds the dir to user-level `PATH`. Env vars / flags: `$env:TYPHON_VERSION = 'v0.12.0'`, `$env:TYPHON_INSTALL_DIR = 'C:\Tools\Typhon'`, or `.\install.ps1 -Version v0.12.0 -InstallDir C:\Tools\Typhon -NoPath`.

### Build from source

FreeBSD, MUSL Linux, Windows ARM64, or development:

```bash
cd tyc
cargo build --release    # ./target/release/tyc
```

Requires Rust 1.94+. CPython 3.13+ is required at runtime to *execute* emitted code; `tyc` itself does not need Python.

### Code signing

- **macOS**: ad-hoc signed (`codesign --sign -`) — avoids the `killed: 9` failure mode on Apple Silicon but doesn't establish Gatekeeper trust.
- **Windows**: not yet Authenticode-signed; SmartScreen may warn on manual download.
- **Linux**: unsigned (verify SHA-256).

### Uninstall

```bash
rm $HOME/.local/bin/tyc                         # macOS / Linux
Remove-Item "$env:LOCALAPPDATA\Programs\Typhon\tyc.exe"  # Windows (and clean the PATH entry)
```

---

## Part 6 — When to read each section

- **Building a Typhon program that uses `Result` or `go` or `lazy let`** — skim Part 1 to know what gets generated and where.
- **Asking "why does my program work with `tyc run` but fail with `tyc run --compile`?"** — read Part 2.5 (not-supported list) and 2.6 (fallback rule).
- **Setting up a debugger session** — read Part 3 (`.py.map`) and `CLI.md` `tyc debug`.
- **Writing a `.dty` for a foreign Python library** — read Part 4.
- **Installing or upgrading `tyc`** — read Part 5.
