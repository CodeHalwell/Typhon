# 10. Advanced features

The features in this guide are the ones you'll reach for once Typhon is part of your everyday workflow: composing transformations with pipes, computing values at build time with `comptime`, deferring expensive imports with `lazy`, opting into caching with `@pure`/`@memo`, and crossing the boundary into untyped Python with `unsafe` and `.dty` stubs.

## Pipes

A pipe (`|>`) takes the value on its left and threads it into the first positional slot of the call on its right:

```python
let raw: str = "  Hello, World!  "
let cleaned: str = raw |> str.strip() |> str.lower() |> str.replace(",", "")
# "hello world!"
```

Reads top-down: take `raw`, strip it, lowercase it, drop the comma. The same chain without pipes:

```python
let cleaned: str = str.replace(str.lower(str.strip(raw)), ",", "")
```

…which is the standard Python jump-around-and-read-inside-out.

### How `|>` desugars

`a |> f(arg)` is exactly `f(a, arg)`. Left-associative, the pipe argument fills the *first* positional slot:

```python
# Typhon
result = x |> normalise() |> scale(2.0) |> clamp(0.0, 1.0)

# Emitted Python
result = clamp(scale(normalise(x), 2.0), 0.0, 1.0)
```

No partial application, no curry — just position-1 threading.

### When pipes shine

- Long data-cleaning chains (strings, lists, DataFrames).
- Validation pipelines where each step returns the next stage's input.
- Anywhere the inside-out call form is the reading bottleneck.

For one-step transformations, plain `f(x)` is fine.

## `comptime`

`comptime let` bindings are evaluated by the compiler, at build time, and inlined as literals into the emitted Python. The most common use is **env-var validation**:

```python
comptime let PORT: int = int(env("PORT", "8080"))
comptime let DB_URL: str = env("DATABASE_URL")   # build fails if unset
```

```toml
# typhon.toml
[env]
required = ["DATABASE_URL"]
```

What this means:

- `tyc build` reads `$PORT` and `$DATABASE_URL` from the environment.
- If `DATABASE_URL` is missing, the build **fails** at compile time — not at the first request in production.
- The values are inlined as literals in the emitted `.py`:

  ```python
  # Emitted Python
  PORT: int = 8080
  DB_URL: str = "postgresql://..."
  ```

Build-time env validation, by itself, is worth the feature.

### What `comptime` allows

Inside a `comptime` binding (or a `comptime def`) the compiler runs a sandboxed interpreter that supports:

- Pure arithmetic and string operations.
- `env(name, default?)` lookups.
- Simple container construction (`list`, `dict`, `tuple`).
- Calls to other `comptime` functions.

It does **not** allow I/O, subprocesses, network access, random numbers, or imports of arbitrary modules. The sandbox is deliberately small.

### Useful comptime patterns

```python
comptime let BUILD: str = env("BUILD_TAG", "dev")
comptime let IS_PROD: bool = BUILD == "prod"

comptime def feature_flag(name: str) -> bool:
    return env(f"FEATURE_{name.upper()}", "0") == "1"

comptime let DARK_MODE: bool = feature_flag("dark_mode")
```

Each `comptime let` is a *constant* at runtime — there's no runtime cost to checking them, because they're already inlined.

### `comptime let T: type = …` — types as values

A `comptime let` can also hold a *type*, in which case the binding name is substituted wherever the alias is used in annotations:

```python
comptime let IdKind: type = int

def lookup(id: IdKind) -> User?:
    return USERS.get(id)
```

Since v0.9.0 this lowers to a PEP 695 `type IdKind = int` alias statement, and the substitution pass runs before `tyc check` parses the resolved module so check, build, and `tyc run` all see the same shape. Use this when you want a build-switched type without writing two function signatures:

```python
comptime let HashAlgo: type = blake3.Blake3 if FAST_HASH else hashlib.sha256

def fingerprint(data: bytes) -> HashAlgo:
    return HashAlgo(data).digest()
```

### `comptime` inlining works under `tyc run`

Since v0.9.0 the in-process VM also runs the comptime substitution pass — `comptime let PORT = int(env("PORT", "8080"))` produces the same inlined literal under `tyc run` as it does under `tyc build && python build/main.py`. Before v0.9.0 the VM crashed with `NameError: env is not defined` because the substitution didn't run before interpretation.

## Lazy loading

Imports are eager in Python: importing `numpy` runs `numpy/__init__.py` even if you never call into it. For CLIs and short-running scripts, that's wasted startup. `lazy import` defers the work:

```python
lazy import np = numpy

def main() -> None:
    # numpy is not loaded yet
    if len(sys.argv) > 1:
        let arr: np.ndarray = np.array([1, 2, 3])    # loaded here, on first access
```

`np` is a proxy object until you touch an attribute on it. The proxy is **thread-safe** — concurrent first accesses lock around the underlying module load using double-checked locking inside a generated `__TyphonLazy_np_` class.

### What's *not* allowed

`lazy from foo import a, b` is **rejected at parse time**:

```python
lazy from numpy import array         # ❌
```

PEP 690 (which originally proposed deferred imports) notes that `from`-imports eagerly touch attributes on the source module — they defeat lazy loading. The diagnostic redirects you to:

```python
lazy import numpy
# ...
numpy.array(...)
```

### `lazy let` for expensive module-level computation

```python
lazy let CONFIG: Config = load_config_from_disk()
```

This lowers to a sentinel-cached `lazy_let(lambda: load_config_from_disk())` helper emitted into `typhon_runtime`. First access pays the load cost; subsequent accesses are a memory read. Unlike `functools.cached_property` (which is instance-scoped, race-prone, and writable after first evaluation), the module-level helper is robust under concurrency and one-shot. Inside a class body, `lazy let` lowers to `@cached_property` because the per-instance scope is the intended semantics there.

### `lazy` return types

```python
def primes_up_to(n: int) -> lazy[list[int]]:
    let sieve: list[bool] = [True] * (n + 1)
    mut p: int = 2
    while p * p <= n:
        if sieve[p]:
            mut k: int = p * p
            while k <= n:
                sieve[k] = False
                k = k + p
        p = p + 1
    return [i for i in range(2, n + 1) if sieve[i]]
```

`lazy[list[T]]` emits as a generator function instead of materialising the list — useful when the caller may only need a prefix.

## `@pure` and `@memo`

Typhon can verify that a function is **pure**: deterministic, side-effect-free, and safe to cache. The six conditions are:

1. **Synchronous.** No coroutines or generators.
2. **Hashable parameters.** Primitives, frozen dataclasses, tuples of hashables.
3. **No I/O.** No `open`, `socket`, `subprocess`, `print`, logger, DB. Unsafe calls count as impure unless their stub is annotated `@pure`.
4. **No non-determinism.** No `time.*`, `random.*`, `uuid.*`, `os.urandom`.
5. **No mutable module state.** Reads from `comptime let` are fine; reads from a module `mut` are not.
6. **No exceptions.** Pure functions express failure through `Result[T, E]`.

When all six hold, the analyser **may** emit `@functools.cache` — but only when you opt in:

```python
@pure
def normalise(s: str) -> str:
    return s.strip().lower()

@memo
def fib(n: int) -> int:
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)

@pure(memo=True)
def hash_password(salt: str, pw: str) -> str:
    ...
```

- `@pure` *asserts* purity — the compiler verifies the six conditions and raises a hard error if any fail.
- `@memo` triggers the cache insertion; the function must qualify as pure.
- `@pure(memo=True)` is the combined form.

### Project-wide auto-memoisation

```toml
[strictness]
auto-memoise = true
```

With this on, any function the analyser infers as pure gets `@functools.cache` automatically. **Disabled by default.** Caches extend the lifetime of every argument and return value, which is not a transparent change — opt in deliberately.

### What `@memo` emits

```python
# Typhon
@memo
def fib(n: int) -> int: ...

# Emitted Python
import functools

@functools.cache
def fib(n: int) -> int: ...
```

For bounded caches, use `@memo(max=128)`, which lowers to `@functools.lru_cache(maxsize=128)`.

## `unsafe` blocks

When you genuinely need to talk to untyped Python — a vendor library without stubs, exploratory glue code — wrap it in an `unsafe:` region:

```python
import some_messy_lib

def main() -> None:
    unsafe:
        let data = some_messy_lib.fetch()    # the `unsafe:` region marks the dynamic boundary
        let first = data[0]
        let tag = first.get("tag")

    # at the unsafe boundary, you must re-assert the type
    let tag_str: str = str(tag)              # explicit cast
```

What's special:

- Inside `unsafe:`, expressions that would normally infer `Any` bind freely.
- Values acquire a hidden `Unsafe[T]` marker — visible in diagnostics, not in source.
- An `Unsafe[T]` cannot cross out of the block into a non-`unsafe` context expecting a concrete `T`. You must re-assert: annotate, narrow, or cast.

The block lowers to `if True:` so existing scope rules apply unchanged. The type checker tracks an `unsafe_depth` counter and suppresses diagnostics inside it.

### When to use `unsafe`

- One-off scripts that talk to an undocumented vendor API.
- The first day you adopt a third-party library, before writing a stub.
- Genuinely dynamic code that resists static typing (e.g. `eval` or RPC stubs).

For anything long-lived, write a `.dty` stub instead.

## `.dty` stubs

`.dty` is Typhon's stub format — the dialect's stricter version of `.pyi`. Write a `.dty` to describe a Python library's API, and the compiler will:

- Check your code against the stub at build time.
- Emit a `.pyi` companion next to the `.py` so mypy/pyright/etc. also benefit.

```python
# src/stubs/redis.dty
class Redis:
    host: str
    port: int

impl Redis:
    def get(key: str) -> str?
    def set(key: str, value: str) -> bool
    def delete(*keys: str) -> int
```

The `Redis` class corresponds to whatever the underlying `redis-py` exposes. Inside your `.ty` files, treat it as a fully-typed Typhon class.

### Drift detection

`tyc check --stubs` compares each `.dty` against the runtime symbols of the module it describes (a Typhon port of mypy's `stubtest`). Drift shows up as:

- Names declared in the stub but missing at runtime.
- Names present at runtime but missing in the stub.
- Signature mismatches between stub and implementation.

Drift surfaces as `tyc::stub_mismatch` diagnostics with the standard severity wiring. For an in-tree implementation (you wrote both the `.ty` and the stub), drift is a code-review fail. For third-party stubs, drift means the library was upgraded; update the stub and re-check.

> A configurable `stub-check` severity knob is on the roadmap but not yet wired into `typhon.toml`. Today drift uses the default diagnostic severity.

## Putting it together

A small but realistic module: a CLI that loads config at build time, lazily imports a heavy dependency, and memoises a hot lookup.

```python
# src/cli.ty
import sys

lazy import np = numpy

comptime let DB_URL: str = env("DATABASE_URL")
comptime let MAX_BATCH: int = int(env("MAX_BATCH", "100"))

class Config:
    db: str
    batch: int

@pure
def parse_arg(raw: str) -> Result[int, str]:
    if raw.isdigit():
        return Ok(int(raw))
    return Err(f"not a number: {raw}")

@memo
def fib(n: int) -> int:
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)

def main() -> None:
    let cfg: Config = Config(db=DB_URL, batch=MAX_BATCH)

    match parse_arg(sys.argv[1] if len(sys.argv) > 1 else "10"):
        case Ok(n):
            let series: list[int] = [fib(i) for i in range(n)]
            let arr: np.ndarray = np.array(series)
            print(arr |> np.diff() |> np.max())
        case Err(msg):
            print(f"bad input: {msg}")

if __name__ == "__main__":
    main()
```

What this brings together:

- `comptime let DB_URL` fails the build if `DATABASE_URL` isn't set — no surprises in prod.
- `lazy import np = numpy` defers the ~150 ms numpy import; if the user passes an invalid arg, numpy never loads.
- `@pure parse_arg` is verifiably pure — no I/O, no globals, no exceptions, errors through `Result`.
- `@memo fib` caches recursive calls; the compiler has verified `fib` qualifies as pure.
- The pipe chain (`arr |> np.diff() |> np.max()`) reads top-down.
- `match` on a `Result` is exhaustive — the compiler enforces both `Ok` and `Err` cases.

## Common mistakes

**Marking a function `@pure` that touches I/O:**

```python
@pure
def fetch(url: str) -> str:
    import urllib.request
    return urllib.request.urlopen(url).read().decode()    # ❌ I/O
```

```
error[tyc::impure_pure_fn]: `fetch` is annotated `@pure` but performs I/O
                            (urllib.request.urlopen)
```

Either drop `@pure` (and lose the memo eligibility) or refactor to take the body as a parameter.

**`comptime` reading a runtime-only value:**

```python
comptime let NOW: float = time.time()    # ❌ comptime sandbox forbids time.*
```

Fix: compute at runtime, cache with `lazy let`.

**Calling `lazy from foo import bar`:**

```python
lazy from numpy import array    # ❌ rejected at parse time
```

Fix: `lazy import numpy`, then use `numpy.array(...)`.

**Writing a decorator factory:**

Python's classic three-layer decorator factory (`def trace(prefix): def decorator(f): def wrapper(*args, **kwargs): ...`) is impossible to fully type in v1 — Typhon doesn't yet model PEP 612 `ParamSpec`. Annotate every layer with `Callable[..., Any]` (the v1 equivalent of "any callable") to satisfy Rule 1 and let the body remain dynamic:

```python
from typing import Callable, Any
import functools

def trace(prefix: str) -> Callable[[Callable[..., Any]], Callable[..., Any]]:
    def decorator(func: Callable[..., Any]) -> Callable[..., Any]:
        @functools.wraps(func)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            print(f"{prefix}: {func.__name__}")
            return func(*args, **kwargs)
        return wrapper
    return decorator

@trace("LOG")
def add(a: int, b: int) -> int:
    return a + b
```

This passes `tyc check` cleanly. The trade-off is that the call site loses the original `add`'s signature for type-checking purposes (it widens to `Callable[..., Any]`). If you need signature preservation, use a single-decorator pattern without the factory layer; if the dynamic behaviour is critical to the operation (e.g. arbitrary `*args`/`**kwargs` passthrough), `Callable[..., Any]` is the right shape and a future `ParamSpec` pass will tighten it.

For decorators that don't need access to the wrapped function (e.g. registry-style decorators), the simpler `def register(f: Callable[..., Any]) -> Callable[..., Any]:` form skips the factory entirely.

**Pulling an `Unsafe[T]` value out of an `unsafe:` block:**

```python
def parse() -> int:
    unsafe:
        let v = messy_lib.get_int()
    return v    # ❌ Unsafe[Any] cannot flow into a concrete `int` context
```

Fix: re-assert inside or at the boundary:

```python
def parse() -> int:
    unsafe:
        let v = messy_lib.get_int()
        let checked: int = int(v)
    return checked
```

## v0.3.0 language additions

Three new constructs shipped in v0.3.0 that remove common Python annoyances. They are designed to stack with everything you already know.

### `newtype Name = Base` — nominal aliases over primitives

Two values can share the same underlying type but mean completely different things. A `UserId` and a `PostId` are both `int`, yet silently swapping them is a bug.

`newtype` lifts a primitive into its own **nominal type** so the mix-up is caught at `tyc check` time:

```python
newtype UserId = int
newtype PostId = int

def fetch_user(id: UserId) -> str: ...
def fetch_post(id: PostId) -> str: ...

let uid: UserId = UserId(42)
let pid: PostId = PostId(7)

fetch_user(uid)      # ✅
fetch_user(pid)      # ❌ tyc::newtype_violation — PostId is not UserId
fetch_user(42)       # ❌ tyc::newtype_violation — bare int is not UserId; use UserId(42)
```

The relationship is **asymmetric**: a `UserId` flows freely *into* an `int`-typed slot (the underlying value IS an int at runtime), but a bare `int` never satisfies a `UserId`-typed slot without explicit wrapping:

```python
def log_id(raw: int) -> None:
    print(raw)

log_id(uid)          # ✅ UserId escapes upward into int
```

**Emitted Python:**

```python
UserId = NewType("UserId", int)
PostId = NewType("PostId", int)
```

At runtime the constructor `UserId(x)` is the identity function — zero overhead.

**Common use cases:** ID kinds, currency tags (`Usd`, `Eur`), validated strings (`Email`, `Slug`), internal-vs-external markers.

**Tip:** `newtype` works over any primitive (`int`, `str`, `float`, `bytes`). For richer ADTs with multiple fields, use a `frozen class` instead.

---

### `freeze let` — deep-immutable bindings

`let` makes the binding immutable (you cannot rebind the name), but it does not prevent mutation of the value itself — `let cfg: dict[str, int]` lets you write `cfg["port"] = 9000`.

`freeze let` locks both the binding and the value:

```python
freeze let CONFIG = {
    "port": 8080,
    "hosts": ["api.example.com", "api2.example.com"],
}

CONFIG = {}                       # ❌ tyc::immutable_assign — binding is locked
CONFIG["port"] = 9000             # ❌ TypeError at runtime — MappingProxyType
CONFIG["hosts"].append("new")     # ❌ AttributeError at runtime — tuple, not list
```

The deep-freeze is applied at module import time by `typhon_runtime.freeze.deep_freeze`:

| Python value | Frozen equivalent |
|---|---|
| `dict` | `types.MappingProxyType` |
| `list` | `tuple` |
| `set` | `frozenset` |
| `frozen` dataclass | unchanged |
| anything else | `TypeError` at startup if not already immutable |

**Emitted Python:**

```python
CONFIG = __typhon_freeze__({"port": 8080, "hosts": ["api.example.com", "api2.example.com"]})
```

**When to use `freeze let` vs `comptime let`:**

| | `comptime let` | `freeze let` |
|---|---|---|
| Evaluated | at build time | at import time |
| Inlined as literal | ✅ yes | ❌ no — value preserved at runtime |
| Complex expressions | ❌ sandbox limits | ✅ any Python expression |
| Mutable-value protection | ❌ just a name lock | ✅ deep recursion |

Reach for `comptime` for build-time constants (ports, feature flags, API versions). Reach for `freeze let` for large look-up tables and configuration dicts that are expensive to build but must be immutable.

**Current limitation:** `freeze let` is module-level only; class-body use is planned for a future release.

**Check-time validation (v0.9.0):** the type checker pre-validates the RHS of every `freeze let X = <expr>` binding. Constructing a non-`frozen` user class on the RHS now fires `tyc::freeze_not_freezable` at check time instead of letting the failure surface as a runtime `TypeError` at first import:

```python
class Counter:                       # not frozen
    value: int

freeze let CFG = Counter(value=0)    # ❌ tyc::freeze_not_freezable

class Counter frozen:                # ✅ declare the class frozen first
    value: int

freeze let CFG = Counter(value=0)    # ✅
```

**VM behaviour (v0.9.0):** the in-process VM (`tyc run`) also recursively wraps `list → tuple`, `dict → mappingproxy-tagged dict`, `set → frozenset` now, so mutators on a frozen value raise the same `TypeError` CPython's `MappingProxy` does under both execution modes. Before v0.9.0 the VM treated `freeze let` as a regular `let` — mutations through aliased references silently succeeded.

---

### `pub` — module visibility marker

Python's `__all__` is a manual list that drifts out of sync with the module. `pub` is the Typhon alternative: declare what is public at the definition site, and the compiler synthesises `__all__` for you.

```python
pub let API_VERSION: str = "v1"
pub class Client: ...
pub def connect(host: str) -> Client: ...

let _internal_timeout: int = 30   # not exported — no `pub` keyword
```

When at least one name in a module is marked `pub`, the desugarer adds a top-of-file `__all__` that lists every `pub` name:

**Emitted Python (excerpt):**

```python
__all__ = ["API_VERSION", "Client", "connect"]

API_VERSION: str = "v1"
```

`pub` stacks with all other modifiers:

```python
pub class Point frozen:     # public + frozen dataclass
    x: float
    y: float

pub newtype UserId = int    # public newtype

pub model WebhookEvent:     # public Pydantic model
    event: str
    payload: dict[str, str]
```

**Effect on tooling:**
- `from mymodule import *` only imports the `pub` surface.
- Sphinx autoapi, mkdocstrings, and `ty` treat `__all__` as the re-export list.
- IDE re-export inference (pyright, Pyrefly, pylance) respects `__all__` for completions and hover.

**No `pub`? No `__all__`.** If no names are marked `pub`, Typhon does not synthesise `__all__` and the module behaves like any regular Python module (everything importable).

---

## Typing argparse with a dataclass adapter (R3-7)

`argparse.Namespace` is opaque — every `args.foo` reads as `Any`, which
makes the parsed CLI surface a static-typing hole. Typhon doesn't
extend `argparse` directly; instead, declare a Typhon `class Args:`
that describes the parsed shape and write a small adapter that
populates an instance from the namespace.

```python
import argparse

class Args:
    name: str
    count: int = 1
    verbose: bool = False

def parse_cli() -> Args:
    let parser: argparse.ArgumentParser = argparse.ArgumentParser()
    parser.add_argument("--name", type=str, required=True)
    parser.add_argument("--count", type=int, default=1)
    parser.add_argument("--verbose", action="store_true")
    let ns: argparse.Namespace = parser.parse_args()
    return Args(name=ns.name, count=ns.count, verbose=ns.verbose)

def main() -> None:
    let args: Args = parse_cli()
    if args.verbose:
        print(f"name={args.name} count={args.count}")
```

The boundary is the `Args(name=ns.name, ...)` line — every field
crosses from `Any` into the declared type exactly once, and the rest
of the program operates on typed `args`. Combined with `class Args
frozen:` for immutability, this gives you the same static guarantees
you'd get from a hand-written CLI dataclass without pulling in a
third-party library like `pydantic-argparse`.

For a project with multiple subcommands, model each one as a
sealed-union variant:

```python
class BuildArgs:
    out: str
    no_format: bool = False

class CheckArgs:
    paths: list[str]
    stubs: bool = False

type Command = BuildArgs | CheckArgs

def parse_cli() -> Command:
    # … set up subparsers …
    let ns: argparse.Namespace = parser.parse_args()
    if ns.subcommand == "build":
        return BuildArgs(out=ns.out, no_format=ns.no_format)
    return CheckArgs(paths=ns.paths, stubs=ns.stubs)

def main() -> None:
    let cmd: Command = parse_cli()
    match cmd:
        case BuildArgs(out, no_format):
            print(f"building to {out}")
        case CheckArgs(paths, stubs):
            print(f"checking {paths}")
```

The match becomes an exhaustive dispatch over subcommands, with every
field on each variant statically typed.

## What you've learned

- **Pipes** thread a value into the next call's first positional slot.
- **`comptime`** evaluates bindings and functions at build time; fails the build on missing required env vars.
- **`lazy import`** defers module loading until first attribute access; `lazy from` is rejected.
- **`@pure`/`@memo`** verify the six purity conditions and opt into `functools.cache`.
- **`unsafe:`** is the lexical boundary into untyped Python; values must be re-asserted to cross out.
- **`.dty` stubs** describe third-party APIs; `tyc check --stubs` catches drift.
- **`newtype`** (v0.3.0) creates nominal aliases over primitives — prevents silent ID/value mix-ups.
- **`freeze let`** (v0.3.0) deep-freezes module-level values at import time, not just the binding name.
- **`pub`** (v0.3.0) marks names as part of the public API; the compiler synthesises `__all__` automatically.

## Where next

You've walked the whole language. From here, the most useful reading is:

- **[language.md](../language.md)** — the canonical design doc.
- **[configuration.md](../configuration.md)** — every key in `typhon.toml`.
- **[cli.md](../cli.md)** — the full `tyc` subcommand reference, including `tyc trace`, `tyc profile`, and `tyc migrate` (which converts typed Python to Typhon).
- **[roadmap.md](../roadmap.md)** — what's landed, what's next, what's deferred.

Happy compiling.
