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
  recursion (1000-frame default depth since v0.8.0 to match CPython,
  configurable on the `Interpreter`).
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
hashing.

### Native stdlib modules

| Module | What you get |
|---|---|
| `math` | `pi`, `e`, `inf`, `nan`, `sqrt`, `floor`, `ceil`, `log` (with base), `log2`, `log10`, `exp`, `sin`, `cos`, `tan`, `pow`, `fabs`. v0.10.0: `gcd`, `lcm`, `factorial`, `isqrt`, `comb`, `perm` (all reject non-integer args) |
| `os` | `getenv`, `environ`, `os.path.exists`, `isfile`, `isdir` |
| `sys` | `argv`, `platform`, `version`, `exit(code)` |
| `json` | `dumps`, `loads` (full JSON 7159 surface). v0.10.0: `dumps(indent=…)` pretty-prints |
| `time` | `time()`, `sleep()`, `monotonic()`. v0.10.0: `perf_counter()`, `process_time()` |
| `random` | `random()`, `randint(a, b)`, `seed(n)` — xorshift PRNG, NOT cryptographic |
| `enum` (v0.11.0) | `enum.Enum`, `enum.auto()` — backs the `enum Name:` keyword. Members materialise in declaration order, iteration is ordered, `ClassName.MEMBER` repr matches CPython (`<Shape.CIRCLE: 1>`) |
| `datetime` (v0.11.0) | `datetime.datetime(y, mo, d, ...)`, `.now()`, `.fromisoformat(...)`, `.isoformat()`, `+ timedelta`, comparisons, `timedelta(seconds=...)` arithmetic. Naïve / UTC only; tz-aware arithmetic needs `--compile` |
| `re` (v0.8.0, capture groups in v0.11.0) | `match`, `search`, `findall`, `sub`, `split`, `compile`. `match` is anchored at the start of the string. Some flag arguments (`re.MULTILINE`, etc.) are accepted but ignored — `tyc::python_semantic_drift` warns when the impact would change behaviour. v0.11.0: `re.Match.group(n)` / `.groups()` / `.groupdict()` return real capture groups (prior shim returned the whole match for every index) |
| `typing` (v0.8.0) | Generic constructors used at runtime are no-ops; `Callable`, `List`, etc. are accepted in import position and ignored at runtime. Type-only imports are stripped by the desugar pre-pass |
| `collections` (v0.8.0, defaultdict in v0.11.0) | `OrderedDict`, `defaultdict` (v0.11.0: `factory` is actually invoked on missing-key access via the subscript `__missing__` hook, so `dd[k] += 1` works), `Counter`, `namedtuple` |
| `functools` (v0.8.0) | `lru_cache`, `cache`, `cached_property`, `reduce`, `partial` |
| `itertools` (v0.8.0) | `chain`, `count`, `cycle` (materialise a bounded prefix), `accumulate`, `combinations`, `permutations`, `product`, `islice`, `takewhile`, `dropwhile`, `groupby` |
| `dataclasses` (v0.8.0) | `dataclass`, `field`, `fields`, `asdict`, `astuple`. v0.9.0: `field(default_factory=list)` actually invokes the factory per instance — `tags: list[str] = []` no longer shares one list across every instance |
| `pathlib` (v0.8.0, expanded v0.11.0) | `Path` with `exists`, `read_text`, `write_text`, `parent`, `name`, `stem`, `suffix`, `with_suffix`, `joinpath` / `/`. v0.11.0: `__truediv__` dispatch (`Path("a") / "b"`), `.suffixes`, `.parts`, and `str(Path(...))` / `repr(Path(...))` match CPython |
| `collections.deque` (v0.9.0) | Rides on `Value::List` via new `popleft` / `appendleft` / `extendleft` / `rotate` list methods. Graph / BFS / queue algorithms work end-to-end |
| `heapq` (v0.9.0) | `heappush`, `heappop`, `heapify`, `heappushpop`, `heapreplace`, `nsmallest`, `nlargest` |
| `contextlib` (v0.9.0) | `@contextmanager` identity decorator and `contextmanager`-decorated factories. `with` block honours the wrapped `__enter__` / `__exit__` shape |
| `pydantic` (v0.9.0, expanded v0.10.0) | `BaseModel` is a placeholder so declaring a `model` doesn't `ImportError`. Since v0.10.0 `Model.model_validate(mapping)` constructs an instance from a dict, `inst.model_dump()` returns a dict of fields in declaration order, and `model_dump_json()` the JSON form — flat `model` classes are usable under `tyc run`. Nested-model validation is not type-directed yet; deeply-nested models still need `--compile` |
| `typhon_runtime` | `Ok`, `Err`, `tasks.spawn`, `lazy.lazy_let`, `lazy.lazy_import` — the `spawn` shim runs synchronously. `Ok` / `Err` carry bound `.map` / `.map_err` / `.and_then` / `.or_else` combinators since v0.9.0 |

Any other module raises `ImportError` with a pointer to `--compile`.

### Built-in methods on values

- `str`: `upper`, `lower`, `strip`, `lstrip`, `rstrip`, `split`,
  `splitlines`, `join`, `replace`, `startswith`, `endswith`, `find`,
  `rfind`, `count`, `isdigit`, `isalpha`, `isalnum`, `isspace`, `isupper`,
  `islower`, `title`, `capitalize`, `swapcase`, `encode`. Since v0.10.0
  also `index`, `rindex`, `center`, `ljust`, `rjust`, `zfill`, `rsplit`,
  `partition`, `rpartition`, `removeprefix`, `removesuffix`, `casefold`,
  `isnumeric`, `istitle`, `expandtabs`. `strip` / `lstrip` / `rstrip`
  now honour their `chars` argument (previously silently ignored), and
  `str.format` stringifies values through a user `__str__`.
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
- `int`: `bit_length`. `float`: `is_integer`.
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

## What the VM does not support yet

Surface as `NotImplementedError` at runtime, with the feature name in the
message:

- `async def` / `await` / `gather:` / `go` — synchronous execution only.
  v0.8.0 surfaces a clear `NotImplementedError` pointing at `tyc build &&
  python build/main.py` as the fallback (previously crashed the
  interpreter).
- **Lazy / unbounded generators.** Finite `yield` / `yield from` work
  since v0.10.0 via eager materialisation, but the worst case
  (`while True: yield`) hits the `GENERATOR_CAP = 1_000_000` ceiling and
  raises a clear `RuntimeError` instead of streaming. Truly lazy /
  unbounded generators still need `tyc build`.
- **`@contextmanager` generators inside `with` blocks.** Eager collection
  runs the generator's setup and teardown at call time, so the `with`
  body can't run between them; the VM emits a clear "use `tyc build`"
  message.
- Template strings (`t"…"`).
- IPython escape commands.
- `with` statements other than `open()` and `contextlib.@contextmanager`-decorated
  factories (basic context-manager protocol since v0.9.0).
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

Tree-walking is intentionally simple. Expect roughly CPython-3.13
performance on arithmetic-heavy microbenchmarks (sometimes faster because
there is no per-call bytecode dispatch overhead, sometimes slower because
the AST nodes are not cache-friendly). On allocation-heavy code (lots of
`list`/`dict` construction) the VM is competitive but uses single-threaded
`Rc` — there is no parallelism today.

A bytecode VM and/or PyO3-backed FFI for unsupported modules are tracked
in `docs/roadmap.md`.

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
interp.root.set("custom", tyc_vm::Value::Int(42));
// … run a parsed module …
```

The crate is in `tyc/crates/tyc-vm`. See `lib.rs` for the public surface.
