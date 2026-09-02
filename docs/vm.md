# The Typhon VM

`tyc-vm` is an in-process tree-walking interpreter that runs `.ty` source
directly without ever emitting Python. It is the **default execution mode**
for `tyc run`. The compile-and-exec path is still available behind
`tyc run --compile`.

## Why a VM?

- Faster iteration on pure-Typhon code: no `build/` directory, no
  `typhon_runtime.py` to write, no CPython process spawn.
- Easier debugging: errors point at `.ty` source directly, not at a
  generated `.py` line that needs `tyc trace` to remap.
- A real REPL story: the VM is the loop `tyc repl` will eventually drive.

The tradeoff is honest. The VM does not bring CPython with it, so anything
that reaches into the Python ecosystem (`numpy`, `requests`, `pydantic`,
`pandas`, …) is unavailable. When the VM hits such an import you get a
clear `ImportError` telling you to re-run with `--compile`.

## Usage

```bash
tyc run src/main.ty           # VM (default)
tyc run                       # VM, resolves src/main.ty from typhon.toml
tyc run --compile             # legacy: build → exec CPython
tyc run --compile --temp      # legacy with ephemeral build dir
```

`--no-vm` is accepted as an alias for `--compile`.

## What the VM supports today

### Language features

- Literals: `int`, `float`, `str`, `bytes`, `bool`, `None`, lists, tuples,
  dicts, sets, ranges, f-strings (with `{x:.2f}`-style format specs for
  width / precision / commas).
- Bindings: `let`, `mut`, plain assignment, annotated assignment,
  augmented assignment (`+=`, `*=`, …), walrus (`:=`), tuple/list
  unpacking with starred targets.
- Control flow: `if` / `elif` / `else`, `while`, `for`, `break`, `continue`,
  `return`, `pass`, `match` (literal / capture / wildcard / sequence /
  class patterns including the native `Ok(x)` / `Err(e)` cases).
- Functions: positional, keyword, default, `*args`, `**kwargs`, closures,
  recursion (a 1000-frame limit by default, matching CPython's
  `sys.getrecursionlimit()`; `sys.setrecursionlimit(n)` moves it, rejecting
  `n < 1` with CPython's own `ValueError`, and exceeding it raises
  `RecursionError`).
- Classes: annotated fields (constructor synthesised at instantiation),
  explicit `__init__`, methods declared inside `class` body, methods
  declared in a sibling `impl Foo:` block (merged on the fly), single
  inheritance, attribute set/get, `isinstance`.
- Decorators: `@pure`, `@dataclass`, `@gatherable`, `@override`, `@final`,
  `@staticmethod`, `@classmethod` — recognised and no-op'd. `@memo`,
  `@cache`, `@lru_cache` — wrap the function in a value-keyed memo cache.
  Other decorators run as regular function calls.
- Error handling: `try` / `except T as e` / `else` / `finally`, `raise`,
  the `Result[T, E]` ADT with native `Ok(...)`, `Err(...)`, and the `?`
  propagation operator.
- Imports: `import`, `from ... import`, `as` aliasing, dotted module
  access. The full list of modules the VM can resolve natively is below.
- Comprehensions: list, set, dict, generator (eagerly materialised in v1).
- Generators: `yield` / `yield from` work under `tyc run` since v0.10.0 via
  eager materialisation — a yield-bearing function runs to completion with
  each yielded value buffered, and the call returns an iterator over the
  collected values (capped at `GENERATOR_CAP = 1_000_000` items). Lazy /
  unbounded generators (`while True: yield`) still need `tyc build`.

### Built-in functions

`print`, `len`, `range`, `str`, `int`, `float`, `bool`, `list`, `tuple`,
`dict`, `set`, `frozenset`, `repr`, `type`, `isinstance`, `abs`, `min`,
`max`, `sum`, `sorted` (incl. `key=` / `reverse=`), `reversed`,
`enumerate`, `zip`, `map`, `filter`, `all`, `any`, `next`, `iter`, `hex`,
`bin`, `oct`, `chr`, `ord`, `round`, `input`, `hash`, `id`, `callable`,
`issubclass` (v1.0.0-beta.1, incl. a tuple of classes),
`open` (read / write / append / binary modes since v0.9.0, plus
`__enter__` / `__exit__` for `with` blocks). Since v0.10.0 also `divmod`
(raises `ZeroDivisionError` with CPython messages), `pow` (2- and 3-arg
modular), `format`, `ascii`, and `int(str, base)` including `base=0`
(autodetect `0x` / `0o` / `0b`). `min` / `max` accept `key=` / `default=`
keyword arguments. Decorator stubs `@property`, `@classmethod`,
`@staticmethod`, and the `super()` call are present as identity-ish
builtins since v0.9.0 so decorated methods no longer crash on import.

Since v0.10.0 `type(x)` returns a **real type object**, not a plain
string. `type(x).__name__` resolves to the type name, `str(type(x))`
renders `<class 'int'>`, and equality holds across the expected cases:
`type(a) == type(b)`, `type(inst) == SomeClass`, `type(5) == int`,
`type(5) == type(6)`. User instances map to their declaring class;
builtins map to cached singleton type objects.

Plus the result constructors `Ok` and `Err` (with the `.map` /
`.map_err` / `.and_then` / `.or_else` combinators bound natively
since v0.9.0), the singletons `True`, `False`, `None`, and the
placeholder bases `object`, `Protocol`, `BaseModel`, `Generic`,
`TypedDict`. `frozenset(...)` is hashable as a dict key since v0.9.0
via a `HashKey::FrozenSet` variant with insertion-order-independent
hashing; a **class object** is hashable by identity since v1.0.0-beta.1,
so a type-keyed registry (`{SomeClass: handler}`,
`functools.singledispatch`) works.

A **function object has its own `__dict__`** since v1.0.0-beta.1:
`wrapper.register = …` and other decorator-published API work as they do
in CPython.

A `class X(NamedTuple)` is a real tuple (indexable, iterable, comparable
with a plain tuple, `X(a=1, b=2)` repr, `_fields` / `_asdict` /
`_replace`) and a `class X(TypedDict)` constructs a plain `dict`, both
since v1.0.0-beta.1 — before that each built an opaque instance, so
`p[0]` and `u["k"]` raised.

### Native stdlib modules

| Module | What you get |
|---|---|
| `math` | `pi`, `e`, `inf`, `nan`, `sqrt`, `floor`, `ceil`, `log` (with base), `log2`, `log10`, `exp`, `sin`, `cos`, `tan`, `pow`, `fabs`. v0.10.0: `gcd`, `lcm`, `factorial`, `isqrt`, `comb`, `perm` (all reject non-integer args) |
| `os` / `os.path` (rebuilt v1.0.0-beta.1) | The process and filesystem surface: `getenv`, `environ`, `getcwd`, `chdir`, `listdir`, `scandir`, `walk`, `mkdir`, `makedirs`, `remove`, `rmdir`, `rename`, `replace`, `stat`, `access`, `getpid`, `cpu_count`, `system`, `strerror`, `urandom`, the `O_*` / `*_OK` / `SEEK_*` constants, `PathLike` / `fspath`, and a full `posixpath` (`join`, `split`, `splitext`, `basename`, `dirname`, `normpath`, `abspath`, `realpath`, `relpath`, `commonpath`, `commonprefix`, `expanduser`, `expandvars`, `isabs`, `exists`, `isfile`, `isdir`, `islink`, `getsize`, `getmtime`, `samefile`). Errors carry CPython's `errno` / `strerror` / `filename`. `import os.path` and `import posixpath` resolve to the same shim |
| `sys` | `argv`, `platform`, `version`, `version_info`, `byteorder`, `maxsize`, `exit(code)`, `stdout`, `stderr`, `stdin`, `getrecursionlimit` / `setrecursionlimit`. v1.0.0-beta.1: `modules` (a live view of the import cache) and `exc_info()` (the exception being handled, with `None` for the traceback slot — the VM has no traceback object). Assigning `sys.stdout` redirects `print`, so `contextlib.redirect_stdout` works |
| `json` | `dumps`, `loads` (full JSON 7159 surface). v0.10.0: `dumps(indent=…)` pretty-prints |
| `time` | `time()`, `sleep()`, `monotonic()`. v0.10.0: `perf_counter()`, `process_time()` |
| `random` | `random()`, `seed(n)`, `getrandbits`, `randint`, `randrange`, `uniform`, `gauss`, `choice`, `shuffle`, `sample` — a **CPython-compatible MT19937**, following `random.py` / `_randommodule.c`, so `random.seed(n)` produces a **byte-identical sequence** under `tyc run` and under `tyc build` + CPython. An *unseeded* program seeds from OS-derived entropy, as CPython does at import, so it draws a different sequence on every run (v1.0.0-alpha.8 — before that the default was a fixed constant, making `tyc run` repeatable where CPython is not). `seed()` / `seed(None)` reseeds from entropy; string / bytes / float seeds are rejected (CPython hashes them through SHA-512) — use `tyc run --compile` for those. NOT cryptographic on either surface: use `secrets` |
| `enum` (v0.11.0) | `enum.Enum`, `enum.auto()` — backs the `enum Name:` keyword. Members materialise in declaration order, iteration is ordered, `ClassName.MEMBER` repr matches CPython (`<Shape.CIRCLE: 1>`) |
| `datetime` (v0.11.0) | `datetime.datetime(y, mo, d, ...)`, `.now()`, `.fromisoformat(...)`, `.isoformat()`, `+ timedelta`, comparisons, `timedelta(seconds=...)` arithmetic. Naïve / UTC only; tz-aware arithmetic needs `--compile` |
| `re` (v0.8.0, capture groups in v0.11.0) | `match`, `search`, `findall`, `sub`, `split`, `compile`. `match` is anchored at the start of the string. Some flag arguments (`re.MULTILINE`, etc.) are accepted but ignored — `tyc::python_semantic_drift` warns when the impact would change behaviour. v0.11.0: `re.Match.group(n)` / `.groups()` / `.groupdict()` return real capture groups (prior shim returned the whole match for every index) |
| `typing` (v0.8.0) | Generic constructors used at runtime are no-ops; `Callable`, `List`, etc. are accepted in import position and ignored at runtime. Type-only imports are stripped by the desugar pre-pass |
| `collections.abc` | The abstract container types (`Callable`, `Iterable`, `Iterator`, `Generator`, `Sequence`, `Mapping`, `MutableMapping`, `Set`, `Hashable`, `Awaitable`, `Coroutine`, `AsyncIterator`, the `*View`s, …) as identity natives — annotation-only at runtime, mirroring the `typing` shim |
| `abc` (v0.15.6) | `ABC`, `ABCMeta`, `abstractmethod`, `abstractclassmethod`, `abstractstaticmethod`, `abstractproperty`, `update_abstractmethods` — identity natives; a non-class base such as `ABC` is ignored at class creation, so `class H(ABC): @abstractmethod def handle(...)` runs |
| `asyncio` | The cooperative-sequential shim: `run`, `gather` (incl. `return_exceptions=True`), `TaskGroup` (`create_task`, `__aenter__` / `__aexit__`), `sleep` (real wall-clock), `timeout` (checked at scope exit), `Queue` (fails loudly instead of deadlocking), and the exception classes (`TimeoutError`, `CancelledError`, …). Coroutines are forced to completion at their `await` — see "What the VM does not support yet" below for the interleaving caveat |
| `collections` (v0.8.0, defaultdict in v0.11.0) | `OrderedDict`, `defaultdict` (v0.11.0: `factory` is actually invoked on missing-key access via the subscript `__missing__` hook, so `dd[k] += 1` works), `Counter`, `namedtuple` |
| `functools` (v0.8.0) | `lru_cache`, `cache`, `cached_property`, `reduce`, `partial`, `wraps`. v1.0.0-beta.1: `partial` binds keyword arguments too (`partial(pow, exp=2)`), plus `cmp_to_key`, `total_ordering` and `singledispatch` |
| `itertools` (v0.8.0) | `chain`, `count`, `cycle` (materialise a bounded prefix), `accumulate`, `combinations`, `permutations`, `product`, `islice`, `takewhile`, `dropwhile`, `groupby` |
| `dataclasses` (v0.8.0) | `dataclass`, `field`, `fields`, `asdict`, `astuple`. v0.9.0: `field(default_factory=list)` actually invokes the factory per instance — `tags: list[str] = []` no longer shares one list across every instance |
| `pathlib` (v0.8.0, rebuilt v1.0.0-beta.1) | `PurePath` / `Path` over the `os` shim: construction and normalisation, `parts`, `parent`, `parents`, `name`, `stem`, `suffix`, `suffixes`, `with_name` / `with_stem` / `with_suffix`, `joinpath` / `/`, `relative_to`, `is_absolute`, `match`, `as_posix`, `as_uri`, comparison and hashing, `home` / `cwd` / `absolute` / `resolve` / `expanduser`, `exists` / `is_file` / `is_dir` / `stat`, `read_text` / `write_text` / `read_bytes` / `write_bytes` / `open`, `iterdir` / `glob` / `rglob` / `walk`, `mkdir` / `touch` / `unlink` / `rmdir` / `rename`. `repr` and error messages match CPython |
| `collections.deque` (v0.9.0) | Rides on `Value::List` via new `popleft` / `appendleft` / `extendleft` / `rotate` list methods. Graph / BFS / queue algorithms work end-to-end |
| `heapq` (v0.9.0) | `heappush`, `heappop`, `heapify`, `heappushpop`, `heapreplace`, `nsmallest`, `nlargest` |
| `contextlib` (v0.9.0) | `@contextmanager` identity decorator and `contextmanager`-decorated factories. `with` block honours the wrapped `__enter__` / `__exit__` shape. v1.0.0-beta.1: `@asynccontextmanager` really drives its generator (it was an identity decorator, so `async with` raised), plus `suppress`, `nullcontext`, `closing`, `redirect_stdout`, `redirect_stderr`, `ExitStack` |
| `pydantic` (v0.9.0, expanded v0.10.0) | `BaseModel` is a placeholder so declaring a `model` doesn't `ImportError`. Since v0.10.0 `Model.model_validate(mapping)` constructs an instance from a dict, `inst.model_dump()` returns a dict of fields in declaration order, and `model_dump_json()` the JSON form — flat `model` classes are usable under `tyc run`. Nested-model validation is not type-directed yet; deeply-nested models still need `--compile` |
| `io` (v1.0.0-beta.1) | `open` and the file objects behind it (`TextIOWrapper`, `BufferedReader` / `BufferedWriter`, `FileIO`), `StringIO`, `BytesIO`, `SEEK_*`, `UnsupportedOperation`. Modes, encodings, newline translation, `seek` / `tell` / `truncate` / `flush`, iteration by line, and the CPython error messages for a closed or wrong-mode file |
| `shutil` (v1.0.0-beta.1) | `copy`, `copy2`, `copyfile`, `copytree`, `move`, `rmtree`, `which`, `disk_usage`, `SameFileError` |
| `tempfile` (v1.0.0-beta.1) | `mkdtemp`, `mkstemp`, `gettempdir`, `NamedTemporaryFile`, `TemporaryDirectory`, `TemporaryFile` |
| `glob` (v1.0.0-beta.1) | `glob`, `iglob`, `escape`, `has_magic` — the same matcher `pathlib.Path.glob` uses, including `**` |
| `hashlib` (v1.0.0-beta.1) | `md5`, `sha1`, `sha224`, `sha256`, `sha384`, `sha512`, `blake2b`, `blake2s`, `new`, with `update` / `digest` / `hexdigest` / `copy` and the `digest_size` / `block_size` / `name` attributes |
| `base64` (v1.0.0-beta.1) | `b64encode` / `b64decode` (incl. `altchars`), `urlsafe_*`, `standard_*`, `b32encode` / `b32decode`, `b16encode` / `b16decode`, `encodebytes` / `decodebytes` |
| `csv` (v1.0.0-beta.1) | `reader`, `writer`, `DictReader`, `DictWriter`, the `QUOTE_*` policies, `excel` / `excel-tab` / `unix` dialects and `register_dialect`. Quoted fields spanning lines are parsed. No `Sniffer`, no `escapechar` escaping |
| `string` (v1.0.0-beta.1) | The constant tables (`ascii_letters`, `digits`, `punctuation`, …), `capwords`, `Template` (`substitute` / `safe_substitute`) |
| `operator` (v1.0.0-beta.1) | The function forms of every operator plus `itemgetter`, `attrgetter`, `methodcaller`, `countOf`, `indexOf`, `length_hint` |
| `bisect` (v1.0.0-beta.1) | `bisect_left`, `bisect_right`, `insort_left`, `insort_right` (and the `bisect` / `insort` aliases), with `lo` / `hi` / `key` |
| `__future__` (v1.0.0-beta.1) | The feature flags, so `from __future__ import annotations` imports rather than raising |
| `asyncio` additions (v1.0.0-beta.1) | `to_thread` (runs the call inline — there is no other thread under the sequential scheduler), and `Lock` / `Semaphore` / `BoundedSemaphore` / `Event`. Acquisition always succeeds at once, since two coroutines are never inside the same critical section; `Event.wait()` on an unset event fails loudly rather than deadlocking, like `Queue.get` on an empty queue |
| `typhon_runtime` (and `typhon_runtime.*`) | `Ok`, `Err`, `tasks.spawn`, `lazy.lazy_let`, `lazy.lazy_import` — the `spawn` shim runs synchronously. `Ok` / `Err` carry bound `.map` / `.map_err` / `.and_then` / `.or_else` combinators since v0.9.0. Submodule imports (`from typhon_runtime.freeze import deep_freeze`) resolve to the matching submodule |

`enum` is resolved separately by the interpreter (it backs the `enum` keyword), not through the module table.

**Anything outside this set takes the compiled path automatically**
(v1.0.0-beta.1). `tyc run` scans the program's imports *before* executing
anything; if one names a module the VM does not model, it prints a `note:`
saying which, then builds the project and runs it under CPython — exactly
what `tyc run --compile` does. Nothing half-executes and restarts, so the
program's output and exit code are the compiled path's. `--no-fallback`
turns the scan into a hard failure instead (the VM's own
`ModuleNotFoundError`), which is what you want for a hermetic run or to
find out whether the VM covers a program.

Modules the VM deliberately does **not** model, because they need a real
CPython runtime rather than a shim: `sqlite3`, `subprocess`, `threading`
/ `multiprocessing`, `socket` / `urllib` / `http`, `ctypes`, `decimal`,
`fractions`, `logging`, `configparser`, `struct`, and every third-party
package (`pydantic` has the placeholder above; `numpy`, `requests`,
`yaml`, … do not). Importing one is not an error — it is a slower run.

### Built-in methods on values

- `str`: `upper`, `lower`, `strip`, `lstrip`, `rstrip`, `split`,
  `splitlines`, `join`, `replace`, `startswith`, `endswith`, `find`,
  `rfind`, `count`, `isdigit`, `isalpha`, `isalnum`, `isspace`, `isupper`,
  `islower`, `title`, `capitalize`, `swapcase`, `encode`. Since v0.10.0
  also `index`, `rindex`, `center`, `ljust`, `rjust`, `zfill`, `rsplit`,
  `partition`, `rpartition`, `removeprefix`, `removesuffix`, `casefold`,
  `isnumeric`, `istitle`, `expandtabs`. `strip` / `lstrip` / `rstrip`
  now honour their `chars` argument (previously silently ignored), and
  `str.format` stringifies values through a user `__str__`. Since
  v1.0.0-beta.1 also `isascii`, `isidentifier`, `isprintable`.
- `list`: `append`, `extend`, `insert`, `pop`, `remove`, `index`, `count`,
  `clear`, `reverse`, `sort`, `copy`. Since v0.10.0 `sort` accepts
  `reverse=` / `key=` kwargs and honours a user `__lt__` / `__eq__`;
  `index` / `count` / `remove` and `in` membership use `__eq__`-aware
  comparison instead of identity.
- `dict`: `get`, `keys`, `values`, `items`, `pop`, `update`, `setdefault`,
  `clear`, `copy`. Since v0.10.0 `dict(other_dict)` shallow-copies and
  `dict(**kwargs)` works.
- `set`: `add`, `remove`, `discard`, `clear`, `copy`. Plus the binary
  operators `|`, `&`, `-`, `^`. Since v0.10.0 also the named forms
  `union`, `intersection`, `difference`, `symmetric_difference`,
  `issubset`, `issuperset`, `isdisjoint`, `update`. `frozenset` preserves
  the frozen sentinel through `union` / `intersection` / `difference` /
  `symmetric_difference`; `update` is rejected on frozensets.
- `tuple`: `count`, `index`.
- Set comparison is subset / superset (`{1} <= {1, 2}`), `d1 | d2` merges
  two dicts (PEP 584), a value-mixin enum member is its value under
  arithmetic and `int()` (`-Colour.RED`, `int(Colour.RED)`), and
  `Perm.READ in (Perm.READ | Perm.WRITE)` is a flag bit-test — all
  v1.0.0-beta.1; a bare `1 in 3` still raises, as in CPython.
- `bytearray` (v1.0.0-beta.1): the mutable sibling of `bytes` —
  construction from bytes / an int / an iterable of ints / `str` plus an
  encoding, `append` / `extend` / `insert` / `pop` / `remove` / `clear` /
  `reverse` / `copy`, index and slice read *and* write, `+` / `*` /
  `in` / comparison against `bytes`, the `bytes` read methods, and
  `bytearray.fromhex`. `repr` is CPython's `bytearray(b'…')`, and it is
  unhashable, as CPython's is. `x in b"…"` (a byte value or a
  subsequence) works too — the VM rejected both.
- `int`: `bit_length`, `bit_count`, `to_bytes`, and the classmethod
  `int.from_bytes`. `float`: `is_integer`. Both carry the `numbers`
  tower's read-only components since v1.0.0-beta.1 — `real`, `imag`,
  `conjugate()`, plus `numerator` / `denominator` on `int`.
- Every builtin *type* also exposes its methods unbound, as CPython does:
  `str.upper(s)`, `dict.get(d, k)`, `list.count(xs, x)` (v1.0.0-beta.1),
  plus the classmethods `int.from_bytes` and `bytes.fromhex`.
- `float.hex()` and `int` / `float` `as_integer_ratio()` (v1.0.0-beta.1).
- `str.format` handles the `!r` / `!s` / `!a` conversions and the
  `{0.attr}` / `{name[key]}` accessors (v1.0.0-beta.1); `str.center`
  biases its extra pad character the way CPython does.
- A class exposes `__doc__` and `__mro__`, and a function
  `__type_params__` (v1.0.0-beta.1).
- `bytes`: `repr` matches CPython byte-for-byte (`b'hi'` by default,
  `b"with 'embedded'"` fallback, `\xNN` for non-printable bytes). Since
  v0.11.0 also `decode`, `hex`, `fromhex`, `count`, `find`, `rfind`,
  `startswith`, `endswith`, `split`, `strip`.
- `complex` (v0.11.0): `Value::Complex(re, im)` is a real VM value.
  `complex(re, im)` / `complex("1+2j")` construct it, the
  reflected dunders (`__radd__` / `__rmul__` / …) dispatch, arithmetic
  promotes from int / float, and `complex` is hashable so it works as
  a dict / set key.

### Dunder dispatch on user instances (v0.10.0)

Before v0.10.0 the VM only dispatched dunders for native types. It now
honours them on user-class instances:

- **Operator overloading.** `__add__` / `__sub__` / `__mul__` /
  `__truediv__` / `__floordiv__` / `__mod__` / `__pow__` / `__matmul__` /
  `__lshift__` / `__rshift__` / `__and__` / `__or__` / `__xor__` plus the
  reflected `__radd__` / `__rmul__` / … forms. Previously `a + b` for two
  user instances raised `TypeError: unsupported operand type`.
- **Rich comparisons.** `__eq__` / `__ne__` / `__lt__` / `__le__` /
  `__gt__` / `__ge__` dispatch, and feed `in` / `list.index` /
  `list.count` / `list.remove` / `list.sort`.
- **`__str__` / `__repr__`** are honoured by `print` / `str` / `repr` /
  f-strings. **`__len__` / `__getitem__` / `__contains__`** dispatch on
  the matching builtin call.
- **`@property` getters** are invoked on attribute read; **`@classmethod`**
  binds the class as `cls`. Both are inherited through bases, and the
  descriptor marker is cleared when a subclass plain method overrides an
  inherited property / classmethod.

## Multi-file projects

Since v0.9.0 the VM loads sibling `.ty` modules from the project source
root, honours relative imports (`from .repo import x`), and caches each
module's bindings as a `Value::Module`. So a project laid out as

```
src/
  main.ty
  repo/
    __init__.ty
    users.ty
```

with `from .repo.users import load` in `main.ty` runs under `tyc run`
without any `.py` emission. `tyc run --compile` now spawns
`python -m <pkg>.main` instead of `python build/main.py` so relative
imports in the entry point resolve correctly.

## Comptime, freeze, lazy

- `comptime let X = ...` inlines in the VM via the substitution pass
  shared with `tyc build`. `comptime let PORT = int(env(...))` no
  longer crashes with `NameError: env is not defined` under `tyc run`
  (v0.9.0).
- `freeze let CFG = {...}` actually freezes the value: `list → tuple`,
  `dict → mappingproxy-tagged dict`, `set → frozenset`, recursive.
  Mutations through aliased references raise the same `TypeError`
  CPython's `MappingProxy` does (v0.9.0). The check pass also
  pre-validates the RHS so non-`frozen` user-class constructors fail
  at `tyc check` time via the new `tyc::freeze_not_freezable`
  diagnostic instead of at first import.
- `lazy import np = numpy` uses the simpler `import M as N` rewrite in
  VM mode (the descriptor-based proxy class the build path emits has
  nothing to bind against in a tree-walking VM) (v0.9.0).

## Verifying the drop-in contract

The VM is contractually a drop-in for `tyc build` + CPython: any difference in
stdout or exit code between the two is a bug, not a documented limitation.
`scripts/vm-differential.sh` is the CI gate that holds that contract — it builds
and executes every unit in `examples/` and `stress/` both ways and diffs the
results. Divergences that exist today are pinned in
`scripts/differential-baseline.txt`, which the gate forces to only ever shrink.
See [differential-testing.md](differential-testing.md); every entry in that
baseline is a VM bug awaiting a fix.

## What the VM does not support yet

Surface as `NotImplementedError` at runtime, with the feature name in the
message:

- **Real (interleaved) concurrency.** The VM now runs async programs via a
  *cooperative sequential* scheduler: calling an `async def` produces a
  coroutine thunk (the body does not run — matching CPython); `await`,
  `asyncio.run`, `asyncio.gather` (including `return_exceptions=True`),
  `TaskGroup.create_task`, and `typhon_runtime.tasks.spawn` (the `go`
  lowering) force it to completion in program order. `gather:` blocks,
  `go f(x) -> task` + `await task`, `async for` over an async generator or
  a hand-written `__aiter__` / `__anext__` iterator, `async with` (over a
  user `__aenter__` / `__aexit__` **or** an `@asynccontextmanager`
  generator), `asyncio.sleep`
  (real wall-clock sleep), `asyncio.timeout` (checked at scope exit),
  and `asyncio.Queue` all work — with results identical to CPython
  whenever the program's correctness doesn't depend on task
  *interleaving*. Programs that do depend on it (e.g. producer/consumer
  pairs that hand off through a queue mid-flight) fail **loudly**:
  `Queue.get` on an empty queue / `put` past `maxsize` raise a
  `RuntimeError` naming `tyc run --compile` instead of deadlocking.
  A body that overruns `asyncio.timeout` raises `TimeoutError` at scope
  exit — after its side effects, unlike a real cancellation.
  `@asynccontextmanager` factories share the eager-generator limitation
  below.
- **`ExceptionGroup` / `except*`.** Modelled since v1.0.0-alpha.7. A
  failing task inside a `gather:` block or a `TaskGroup` surfaces as
  `ExceptionGroup('unhandled errors in a TaskGroup', [...])` raised from
  `__aexit__`, exactly as under CPython — so it must be handled with
  `except*`, not a plain `except ValueError:`. Before that release the VM
  ignored `is_star` entirely and raised the bare exception out of
  `create_task`, which a plain `except` caught under `tyc run` and did not
  under `tyc build && python`, from the same clean `tyc check`. `except*`
  now splits the group, runs every matching handler once with its own
  recursively-split subgroup, re-raises the unmatched remainder, and raises
  the same `TypeError` CPython does for `except* ExceptionGroup`.
  A naked `raise` inside an `except*` handler reconstitutes the original
  group (CPython's `_PyExc_PrepReraiseStar` re-raise merging); `await` on a
  failed task re-raises the task's exception; `KeyboardInterrupt` /
  `SystemExit` escape `TaskGroup.__aexit__` bare rather than grouped; and
  split sides re-derive through `BaseExceptionGroup.__new__`'s downcast
  rule, so `isinstance(e, Exception)` holds inside an `except*` handler
  over a mixed group exactly as under CPython.

  Three residual divergences: `.split()` / `.subgroup()` / `.derive()` are
  not exposed as user-callable methods, `__context__` is not chained
  between exceptions collected from handler bodies, and an uncaught group
  prints the summary line (`ExceptionGroup: g (2 sub-exceptions)`) rather
  than CPython's nested `+-+---- 1 ----` traceback tree. Inherent to
  sequential execution: the VM cannot cancel sibling tasks, so a
  multi-failure `gather:` may report more members than CPython would — and
  the body of a `TaskGroup` sees a failed task's exception at the `await`
  rather than the `CancelledError` a real cancellation would deliver.
- **Lazy / unbounded generators.** Finite `yield` / `yield from` work
  since v0.10.0 via eager materialisation, but the worst case
  (`while True: yield`) hits the `GENERATOR_CAP = 1_000_000` ceiling and
  raises a clear `RuntimeError` instead of streaming. Truly lazy /
  unbounded generators still need `tyc build`.
- **`generator.send()` / coroutine-style generators.** Because generators
  are materialised eagerly (above), the VM has no live frame to resume, so
  `gen.send(value)`, `gen.throw(...)`, and the `value = yield x` two-way
  protocol are unsupported — the generator runs to its cap before the
  first `send` could ever reach it. Bidirectional generators need
  `tyc build`. (Plain forward iteration is unaffected.)
- **A generator whose `yield` sits where the tree-walk cannot suspend.**
  Most generator bodies run lazily (`@contextmanager` and
  `@asynccontextmanager` factories included, since v1.0.0-beta.1, so the
  `with` body runs between setup and teardown). A `yield` in a loop test,
  a `with` item, a call argument after another call, or two yields in one
  expression falls back to eager collection, where setup and teardown both
  run at call time.
- Template strings (`t"…"`).
- IPython escape commands.
- A `with` / `async with` over anything that is neither `open()`, a
  `@contextmanager` / `@asynccontextmanager` factory, nor an object
  implementing `__enter__` / `__exit__` (or `__aenter__` / `__aexit__`).
- `lazy let` inside a class body uses an identity decorator; callers must
  use the method-call form `obj.x()`.
- **Nested-model `pydantic.model_validate`** is not type-directed — a
  nested dict stays a dict; deeply-nested models need `tyc build`.

If your program needs any of these, run with `--compile` for now.

**Behaviour additions in v0.11.0** (vs v0.10.0):

- `enum Name:` is a first-class keyword. Bare members auto-fill with
  `enum.auto()`; explicit `MEMBER = value` is preserved. The class
  body materialises members in declaration order and
  `Shape.CIRCLE` repr matches CPython.
- Bare `super()` inside a method is rewritten by `tyc-desugar` to the
  explicit `super(EnclosingClass, self)` form, so
  `@dataclass(slots=True)` (which orphans the `__class__` cell) no
  longer crashes emitted code. Explicit `super(X, y)` calls are left
  untouched.
- `Value::Complex(re, im)` is a real VM value with arithmetic
  promotion across `int` / `float`, reflected dunders, and hash
  support.
- `Value::DictView` backs `dict.keys()` / `.values()` / `.items()`.
  Repr matches CPython (`dict_keys([...])`), iterate, support `len`,
  membership-test with `in`, re-iterable.
- `__call__` dispatches on callable instances; `__post_init__` fires
  after auto-generated construction; multi-level inheritance
  accumulates fields across the full MRO.
- Subscript `__missing__` hook backs `defaultdict`: `dd[k] += 1` works
  via the new `collections.defaultdict` shim.
- Native `datetime` (naïve / UTC) and expanded `pathlib` (with
  `__truediv__` / `.suffixes` / `.parts`).
- `re.Match.group(n)` / `.groups()` / `.groupdict()` return real
  capture groups (prior shim returned the whole match for every
  index).
- `builtins.round` uses banker's rounding (half-to-even).
- `bytes` gains `decode` / `hex` / `fromhex` / `count` / `find` /
  `rfind` / `startswith` / `endswith` / `split` / `strip`.
- `itertools.groupby` honours `key=` instead of grouping by identity;
  `str.split(maxsplit=…)` accepts the pure-keyword form.
- f-string `{x=}` debug conversion renders `x=<repr>`; `str %` and
  f-string `%` percent format types work at runtime.

**Behaviour changes in v0.11.0 — VM value semantics align with CPython**
(these were silent-wrong outputs under v0.10.0; programs that relied
on the old behaviour will see different — correct — results):

- Dataclass instance equality is value-based (same class + all fields
  equal recursively). Class identity uses the underlying `Class`
  pointer so distinct same-named classes from different modules no
  longer collide.
- Dataclass `repr` is `Name(field=value, ...)` in declared field
  order (was `<Name instance>`).
- Dataclass instances are hashable via `HashKey::Instance` (class
  identity + fields sorted by name).
- Set / frozenset equality is order-independent; repr sorts elements
  by canonical key.
- Float `repr` matches CPython's shortest round-tripping form, with
  scientific notation for exp < -4 or ≥ 16 (`e+NN` / `e-NN`, ≥ 2
  exponent digits), `-0.0` preserved.

**Behaviour additions in v0.10.0** (vs v0.9.x):

- Dunder dispatch on user instances: operator overloading + reflected
  forms, rich comparisons, `__str__` / `__repr__` / `__len__` /
  `__getitem__` / `__contains__`, `@property` / `@classmethod`
  (inherited through bases).
- Finite generators (`yield` / `yield from`) via eager materialisation,
  capped at 1M items.
- `type(x)` returns a real type object (`type(x).__name__`,
  `type(x) == int`, `str(type(x))` → `<class 'int'>`).
- Pydantic `model_validate` / `model_dump` / `model_dump_json` for flat
  `model` classes.
- `max` / `min` / `list.sort` accept `key=` / `reverse=` / `default=`
  kwargs.
- Builtins backlog: `divmod`, `pow` (2- and 3-arg), `format`, `ascii`,
  `int(str, base)` (incl. `base=0`), full set algebra, the missing
  string methods, `dict(other)` / `dict(**kwargs)`, `math.gcd` / `lcm`
  / `factorial` / `isqrt` / `comb` / `perm`, `json.dumps(indent=…)`,
  `time.perf_counter` / `process_time`.
- `enumerate` / `zip` / `map` / `filter` no longer panic with
  `RefCell already borrowed` the instant they are iterated.
- `Path.read_text` / `write_text` / `open` tolerate (and ignore)
  `encoding=` / `errors=` kwargs instead of rejecting the call.

**Behaviour additions in v0.9.0** (vs v0.8.x):

- `Result` combinators (`.map`, `.map_err`, `.and_then`, `.or_else`)
  on `Ok` / `Err` work in the VM via bound `NativeFn` wrappers.
- `open()` honours `"w"` / `"a"` / `"wb"` / `"r+"` modes plus
  `__enter__` / `__exit__`. `json.load` / `json.dump` ride on top.
- Class patterns on built-in types (`case str() as s:`,
  `case int() as n:`, …) match in `match` blocks; the exhaustiveness
  pass recognises `case None:` + `case str() as s:` as covering
  `str?`.
- `frozenset(...)` is hashable as a dict key.
- f-string `_` thousands separator works the same way `,` does.
- `bytes` repr matches CPython byte-for-byte.
- Native shims for `collections.deque`, `heapq`, `contextlib.contextmanager`,
  and `pydantic.BaseModel`.
- `@property`, `@classmethod`, `@staticmethod`, `super()` builtins so
  decorated methods don't crash on import.
- Multi-file projects: relative imports and sibling `.ty` modules
  resolve under both `tyc run` and `tyc run --compile`.
- `dataclasses.field(default_factory=list)` invokes the factory
  per instance (no more shared mutable defaults).
- `class!` synthesised `__init__` runs; `except HttpError as e:`
  binds the user `Instance` and exception-type matching walks the
  MRO.
- `freeze let` deeply freezes (list → tuple, dict → mappingproxy,
  recursive); mutators on a frozen dict raise `TypeError` matching
  CPython.
- `comptime let X = ...` inlines via the substitution pass shared
  with `tyc build`.
- `lazy import M = N` uses the simpler `import M as N` rewrite.
- Typed tuple unpack `let (a: int, b: str) = pair()` parses in the VM.

**Behaviour changes in v0.8.0** (vs v0.7.x):

- `Value::Int` is now backed by `num_bigint::BigInt`. `2 ** 100` and
  `fib(99)` compute mathematically-correct results. Programs that
  *relied* on the previous silent i64 wrap-around will now compute
  different results.
- `RcDict` is now an `indexmap::IndexMap`. Dict insertion order is
  preserved across `dict.update`, `del d[k]` (via `shift_remove`),
  and `dict.pop` — so the same `.ty` no longer prints dicts in different
  orders under `tyc run` vs `tyc build && python build/main.py`.
- f-string format flags are fully wired (`{x:0>5}`, `{n:#x}`,
  `{f:.3f}`, `{v:,}`, `{w:>{width}.{prec}f}` etc.) and match CPython
  byte-for-byte.
- Mapping match patterns (`case {"type": "circle"}`,
  `case {…, **rest}`) and sequence-with-star patterns
  (`case [x, *rest, y]`) are implemented.

## Architecture

```
.ty source
    │
    ▼  tyc-syntax::preprocess  (expand `?`, `|>`, `gather:`, `go`, with-chains, lazy)
    ▼  tyc-syntax::parse_module
    │
    ▼  tyc-vm::Interpreter
       ├── env.rs           lexical scope chains with `global`/`nonlocal`
       ├── value.rs         Value enum, hash keys, Python-equality / ordering
       ├── interp.rs        statement and expression evaluators
       ├── builtins.rs      native fns, method dispatch, stdlib modules
       └── ffi.rs           file-handle shim (no PyO3 yet)
```

The VM consumes the **same preprocessed AST** that `tyc build` would
desugar, so any Typhon sugar that lowers cleanly into Python (`?`,
`gather:` once async is in, pipes, with-chains, lazy imports) is the
preprocessor's responsibility. The VM never touches sugar — it sees the
desugared form and interprets it.

`impl Foo:` lowers to `class __typhon_impl_Foo(object):`. The interpreter
recognises the synthetic name and merges the methods straight back into
the live `Foo` class object, so the user-visible model is "methods live
on `Foo`."

## Performance

The tree-walking VM optimizes for correctness/parity with CPython and for
startup latency, not for steady-state compute throughput. On a
hello-world program the VM starts and exits in ~21 ms against ~38 ms for
`tyc build` + a CPython 3.13 process spawn — and the VM skips the build
step entirely, so there's no `build/` directory or `typhon_runtime.py` to
generate first. For short scripts, the REPL, and (eventually) LSP-driven
expression evaluation, the VM wins.

Steady-state compute is the opposite story. Before the Tier 1 work the
VM measured **~5–18× slower** than `tyc build` + CPython 3.13 once fixed
startup cost was factored out (release binary, median of 3 runs, outputs
parity-checked), driven by four architectural costs: every `int` was a
heap-allocated arbitrary-precision `BigInt`, every variable read was a
string-hash `HashMap` lookup walking a parent scope chain, every call
allocated a fresh `Env` (a `HashMap`), and method resolution re-walked
the base-class chain on every miss.

Tier 1 (landed) attacks all four without changing the execution model:

- `Value::Int` wraps a `VmInt` that keeps any `i64`-range value inline
  and only promotes to a heap `BigInt` on overflow — integer arithmetic
  no longer allocates on the common path, and CPython's
  arbitrary-precision semantics are preserved exactly.
- Functions with no `global`/`nonlocal` and no captured closure
  variables resolve locals to fixed slots computed once at definition
  time; other functions keep the `HashMap` path unchanged.
- Method resolution memoises base-chain / negative lookups in a
  per-class cache, and `obj.method(args)` on a user instance dispatches
  directly without building an intermediate `BoundMethod`.

Measured after Tier 1 (same methodology): **~3–14× slower
startup-adjusted, ~2.7–6× end-to-end wall clock**. Tight loops improved
the most (a 3M-iteration accumulator went 2168ms → ~650ms, now ~3×
adjusted); recursive, call-heavy code remains the worst case (~14×
adjusted on a naive recursive `fib`) because each Typhon call still pays
a real Rust frame plus argument binding — exactly the cost Tier 2 (a
bytecode VM) targets. [`docs/vm-performance-plan.md`](vm-performance-plan.md)
has the full measured tables, the root-cause breakdown, and the tiers.

A bytecode VM — the point at which rough CPython parity on most code
becomes realistic — is designed as Tier 2 of that plan but not yet
started; see [`docs/vm-performance-plan.md`](vm-performance-plan.md).
PyO3-backed FFI for unsupported modules is not currently planned; it's
listed there as a non-goal for now.

## Diagnostics

Runtime failures surface as `Traceback (most recent call last):` followed
by `KIND: MESSAGE`. The traceback is intentionally minimal in v1 — it
names the function frame but not the source line. Source-line tracebacks
inside the VM are a tracked follow-up; for now, programs that need full
traceback fidelity should run under `tyc run --compile` and use
`tyc trace` on the captured stderr.

## Talking to the VM from Rust

```rust
use tyc_vm::{Interpreter, run_source};

let code = run_source("print(1 + 2)\n", None)?;   // returns process exit code
// or, for finer control:
let mut interp = Interpreter::new();
// `Value::Int` wraps a `VmInt` (a small-int/`BigInt` two-representation
// integer); build one with `.into()` from any primitive integer.
interp.root.set("custom", tyc_vm::Value::Int(42.into()));
// … run a parsed module …
```

The crate is in `tyc/crates/tyc-vm`. See `lib.rs` for the public surface.
