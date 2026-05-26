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

### Built-in functions

`print`, `len`, `range`, `str`, `int`, `float`, `bool`, `list`, `tuple`,
`dict`, `set`, `frozenset`, `repr`, `type`, `isinstance`, `abs`, `min`,
`max`, `sum`, `sorted` (incl. `key=` / `reverse=`), `reversed`,
`enumerate`, `zip`, `map`, `filter`, `all`, `any`, `next`, `iter`, `hex`,
`bin`, `oct`, `chr`, `ord`, `round`, `input`, `hash`, `id`, `callable`,
`open` (text-read only).

Plus the result constructors `Ok` and `Err`, the singletons `True`,
`False`, `None`, and the placeholder bases `object`, `Protocol`,
`BaseModel`, `Generic`, `TypedDict`.

### Native stdlib modules

| Module | What you get |
|---|---|
| `math` | `pi`, `e`, `inf`, `nan`, `sqrt`, `floor`, `ceil`, `log` (with base), `log2`, `log10`, `exp`, `sin`, `cos`, `tan`, `pow`, `fabs` |
| `os` | `getenv`, `environ`, `os.path.exists`, `isfile`, `isdir` |
| `sys` | `argv`, `platform`, `version`, `exit(code)` |
| `json` | `dumps`, `loads` (full JSON 7159 surface) |
| `time` | `time()`, `sleep()`, `monotonic()` |
| `random` | `random()`, `randint(a, b)`, `seed(n)` — xorshift PRNG, NOT cryptographic |
| `re` (v0.8.0) | `match`, `search`, `findall`, `sub`, `split`, `compile`. `match` is anchored at the start of the string. Some flag arguments (`re.MULTILINE`, etc.) are accepted but ignored — `tyc::python_semantic_drift` warns when the impact would change behaviour |
| `typing` (v0.8.0) | Generic constructors used at runtime are no-ops; `Callable`, `List`, etc. are accepted in import position and ignored at runtime. Type-only imports are stripped by the desugar pre-pass |
| `collections` (v0.8.0) | `OrderedDict`, `defaultdict` (no auto-default — explicit `default_factory` argument is recorded but not invoked), `Counter`, `namedtuple` |
| `functools` (v0.8.0) | `lru_cache`, `cache`, `cached_property`, `reduce`, `partial` |
| `itertools` (v0.8.0) | `chain`, `count`, `cycle` (materialise a bounded prefix), `accumulate`, `combinations`, `permutations`, `product`, `islice`, `takewhile`, `dropwhile`, `groupby` |
| `dataclasses` (v0.8.0) | `dataclass`, `field`, `fields`, `asdict`, `astuple` |
| `pathlib` (v0.8.0) | `Path` with `exists`, `read_text`, `write_text`, `parent`, `name`, `stem`, `suffix`, `with_suffix`, `joinpath` / `/` |
| `typhon_runtime` | `Ok`, `Err`, `tasks.spawn`, `lazy.lazy_let`, `lazy.lazy_import` — the `spawn` shim runs synchronously |

Any other module raises `ImportError` with a pointer to `--compile`.

### Built-in methods on values

- `str`: `upper`, `lower`, `strip`, `lstrip`, `rstrip`, `split`,
  `splitlines`, `join`, `replace`, `startswith`, `endswith`, `find`,
  `rfind`, `count`, `isdigit`, `isalpha`, `isalnum`, `isspace`, `isupper`,
  `islower`, `title`, `capitalize`, `swapcase`, `encode`.
- `list`: `append`, `extend`, `insert`, `pop`, `remove`, `index`, `count`,
  `clear`, `reverse`, `sort`, `copy`.
- `dict`: `get`, `keys`, `values`, `items`, `pop`, `update`, `setdefault`,
  `clear`, `copy`.
- `set`: `add`, `remove`, `discard`, `clear`, `copy`. Plus the binary
  operators `|`, `&`, `-`, `^`.
- `tuple`: `count`, `index`.
- `int`: `bit_length`. `float`: `is_integer`.

## What the VM does not support yet

Surface as `NotImplementedError` at runtime, with the feature name in the
message:

- `async def` / `await` / `gather:` / `go` — synchronous execution only.
  v0.8.0 surfaces a clear `NotImplementedError` pointing at `tyc build &&
  python build/main.py` as the fallback (previously crashed the
  interpreter).
- Real generator functions with `yield` (generator *expressions* and the
  list-comp-shaped form work, but they're materialised eagerly). v0.8.0:
  the same `NotImplementedError` shape applies.
- Template strings (`t"…"`).
- IPython escape commands.
- `with` statements other than `open()` (basic context-manager protocol).
- `lazy let` inside a class body uses an identity decorator; callers must
  use the method-call form `obj.x()`.

If your program needs any of these, run with `--compile` for now.

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
