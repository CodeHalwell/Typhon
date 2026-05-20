---
name: typhon
description: Write, check, build, and migrate code in the Typhon language — a statically-typed, stricter superset of Python that compiles to clean CPython 3.13+ via the `tyc` binary. Use this skill whenever you are editing `.ty` or `.dty` source, modifying `typhon.toml`, invoking any `tyc` subcommand (`build`, `check`, `fmt`, `lsp`, `init`, `migrate`, `repl`, `debug`, `trace`, `profile`, `add`, `remove`, `sync`, `ty`), translating Python to Typhon, debugging Typhon-specific diagnostics, or answering questions about the language, the compiler pipeline, or the project's docs/architecture. Triggers include: any file with a `.ty` / `.dty` / `typhon.toml` extension, the words "Typhon", "tyc", "let/mut binding", "Result[T, E]", "sealed union", "gather:" / "go f(x)", "comptime", "interface", "impl block", "extend", `T?`, `.py.map`, `typhon_runtime`, and any error code matching `tyc::...`.
---

# Typhon — Language, Compiler, and Project Skill

Typhon is **a statically-typed, stricter superset of Python that emits clean CPython 3.13+** with zero runtime dependency on the toolchain. The compiler and language server are a single Rust binary called `tyc`. Every `.ty` file emits valid, idiomatic `.py`. Not all `.py` is valid Typhon.

This skill is the on-the-ground reference for working in this repo: how to write `.ty` correctly, how to read/extend the compiler crates under `tyc/crates/`, how to invoke `tyc`, and how to debug the diagnostics users will hit.

The repo's canonical sources are:

- **`README.md`** — high-level pitch, project status (Phase 0–4+), workspace layout.
- **`docs/long-term-plan.md`** — the source of truth for the design. The narrower docs are excerpts.
- **`docs/language.md`** — type system, error handling, async, `let`/`mut`, comptime.
- **`docs/cli.md`** — every `tyc` subcommand.
- **`docs/configuration.md`** — every key in `typhon.toml`.
- **`docs/architecture.md`** — pipeline + crate layout.
- **`docs/guides/01..10-*.md`** — the teaching surface; read in order the first time.
- **`docs/roadmap.md`**, **`docs/risks.md`**, **`docs/prior-art.md`**, **`docs/follow-ups-2026-05-17.md`** — context for *why* design calls were made.

Whenever this skill and a doc disagree, **the doc wins.** When the docs and an unrelated `.py.map`/emitted-Python detail disagree, the compiler wins — verify with `tyc check`.

---

## When to invoke this skill

Trigger automatically when the working session involves any of:

1. **Authoring or editing `.ty` source.** Always re-read the relevant guide section before writing significant code — the syntax is *close* to Python but five rules quietly diverge (return types are mandatory, `let`/`mut` is mandatory for locals, `T` cannot hold `None`, methods live in `impl`, no implicit `Any`).
2. **Editing `.dty` stubs.** These are the Typhon source of truth for third-party Python APIs; they emit `.pyi` for interop. Drift is caught by `tyc check --stubs`.
3. **Editing `typhon.toml`.** Each strictness flag has subtle defaults — see the [configuration reference](#typhontoml-reference) below.
4. **Working inside the Rust compiler** (`tyc/crates/`). The pipeline is `syntax → resolve → types → analyse → desugar → emit → format`, all backed by a Salsa DB. Each crate has a tight responsibility — see [Compiler architecture](#compiler-architecture).
5. **Migrating `.py` → `.ty`.** Use `tyc migrate` first; it rewrites `Optional[T]` → `T?`, adds `let`/`mut`, drops `@dataclass`. Then resolve diagnostics manually.
6. **Debugging a `tyc::...` diagnostic.** The [diagnostics catalog](#diagnostics-catalog) is the fastest lookup.
7. **Onboarding someone to Typhon.** Walk them through the [Cheat sheet](#cheat-sheet) first, then the guides.

---

## Cheat sheet

The 30-second mental model. Everything else in this skill is detail under one of these bullets.

| Topic | Typhon | Emitted Python |
|---|---|---|
| Local binding | `let x: int = 1` / `mut x: int = 1` | `x: int = 1` |
| Module binding | `X: int = 1` (implicit `let`) or `mut X: int = 1` | `X: int = 1` |
| Nullable | `name: str?` | `name: str \| None` |
| Optional default | `name: str? = None` (no auto-default) | `name: str \| None = None` |
| Class | `class User: id: int` | `@dataclass(slots=True) class User: id: int` |
| Pydantic model | `model ApiUser: id: int` | `class ApiUser(BaseModel): model_config = ConfigDict(extra="forbid"); id: int` |
| Frozen class | `class P frozen: x: float` | `@dataclass(slots=True, frozen=True)` |
| Methods | `impl User: def display(self) -> str: ...` (explicit `self`, then `self.NAME`) | merged into the class body |
| Extend foreign class | `extend User: def x() -> int: ...` | merged at desugar |
| Extend built-in | `extend str: def slug() -> str: ...` | extracted to `__typhon_ext_str__slug` free fn + receiver-typed call rewrites |
| Result type | `Result[T, E]`, `Ok(v)`, `Err(e)` | generated `typhon_runtime.Ok/Err` dataclasses |
| Error propagation | `let n: int = f()?` | inline `isinstance(_t, Err): return _t; n = _t.value` |
| Result chain | `with a = f()?, b = g()?: ...  else err: ...` | sequenced if-isinstance ladder |
| Sealed union | `type Shape = Circle \| Rectangle` | `Shape = Circle \| Rectangle` (just a type alias) |
| Exhaustive match | `match s: case Circle(r): ...` (no `_` needed) | vanilla Python `match` |
| Generic fn | `def first[T](xs: list[T]) -> T?:` | same (PEP 695) |
| Interface | `interface Drawable: def draw() -> None` | `class Drawable(Protocol): ...` |
| Pipe | `a \|> f() \|> g(arg)` | `g(f(a), arg)` |
| Parallel awaits | `gather: a = f(); b = g()` | `async with asyncio.TaskGroup() as _tg: ...` |
| Best-effort gather | `gather(strategy="best-effort"):` | `asyncio.gather(..., return_exceptions=True)` |
| Spawn | `go f(x)` / `go f(x) -> task` | `typhon_runtime.tasks.spawn(...)` (strong-ref registry) |
| Lazy module | `lazy import np = numpy` | bespoke `__TyphonLazy_np_` proxy class with double-checked locking |
| Lazy module-level let | `lazy let CFG: Config = load()` | sentinel-cached `lazy_let(lambda: load())` |
| Lazy class-level let | `lazy let cfg: Config = ...` inside class | `@cached_property` |
| Comptime constant | `comptime let PORT: int = int(env("PORT", "8080"))` | inlined literal at build time |
| Pure assertion | `@pure def f(...) -> T:` | nothing emitted unless `@memo` too |
| Memoised | `@memo def fib(n: int) -> int:` | `@functools.cache` |
| Unsafe boundary | `unsafe: let x = mystery_lib()` | `if True:` (scope-preserving) |
| Guard | `guard x = expr else: return ...` | `if expr is None: return ...; x = expr` |

---

## Five rules every Typhon program follows

These are the rules behind every "but the same code works in Python" surprise.

### Rule 1 — Every parameter and return type is annotated

```python
def add(a: int, b: int) -> int:    # ✅
    return a + b

def add(a, b):                     # ❌ tyc::missing_return_type / implicit Any
    return a + b
```

`-> None` is mandatory for sync functions that return nothing. There is no inference fallback. This is `[strictness] no-implicit-any = true`, which defaults on and you almost never want to turn off.

### Rule 2 — Local bindings declare `let` or `mut`

```python
def demo() -> None:
    let pi: float = 3.14159      # immutable
    mut counter: int = 0         # mutable
    counter = counter + 1        # ✅
    # pi = 3.14                  # ❌ tyc::immutable_assign
```

Module-level bindings default to `let` if you skip the keyword — but a *local* `name = "x"` with no keyword is `tyc::missing_binding_kind`. Reach for `mut` only when you actually rebind.

Carve-outs (no keyword required):
- `global NAME` / `nonlocal NAME` declarations bind the outer-scope variable; the bareword assignment that follows refers to that binding.
- `gather:` block bindings (`gather: a = fetch_a(); b = fetch_b()`) — the keyword itself introduces immutable single-assignment names.
- Walrus operator: `if (n := len(xs)) > 3:` introduces `n` as an implicit `let` binding; rebinding `n` later requires `mut`.

### Rule 3 — `T` cannot hold `None`

```python
def greet(name: str) -> None: ...
def find(id: int) -> str?: ...   # str? == str | None

let found: str? = find(1)
greet(found)                     # ❌ tyc::nullable_use

if found is not None:
    greet(found)                 # ✅ narrowed to str

guard f = found else: return     # ✅ same effect, prettier
greet(f)
```

Narrowing forms the checker understands: `is None`, `is not None`, `isinstance(x, T)`, `guard`, early-return `if x is None: return`. `T?` emits as `T | None`.

### Rule 4 — Methods live in `impl`, not in `class`

```python
class User:
    id: int
    name: str

impl User:
    def display(self) -> str:    # take an explicit `self`; use `self.NAME`
        return f"{self.name} (#{self.id})"
```

Earlier drafts of this guide suggested an implicit-`self` form where bare `name` resolved to `self.name`. That form was never implemented (the resolver can't see the enclosing class's fields by the time it walks the method body), so the explicit `self.NAME` access shown above is what works today. The bare-identifier sugar may come back later; explicit `self` is the durable form.

Writing `__init__` inside `class` is rejected — the constructor is generated. `self` on impl-block methods is currently optional (it's inserted at desugar if omitted, but you can't reach class fields from a no-`self` method), so writing it explicitly is the recommended style.

### Rule 5 — `Any` only enters through `unsafe:` or `.dty` stubs

```python
import messy

let data = messy.fetch()         # ❌ tyc::implicit_any

unsafe:
    let data = messy.fetch()     # ✅ inside the region
let parsed: dict[str, int] = ... # re-assert at the boundary
```

`unsafe:` is a *lexical region*, not a per-value annotation. Values inside acquire a hidden `Unsafe[T]` marker that cannot cross out into a concrete-typed context without a re-assertion (annotation, narrowing, or cast). The block lowers to `if True:` so scope rules are unchanged.

---

## Writing a Typhon program end-to-end

The shortest realistic flow. Pair with `docs/guides/01-hello-world.md`.

```bash
# 1. Build the compiler (one-time; from repo root)
cd tyc && cargo build --release && cd ..
alias tyc="$PWD/tyc/target/release/tyc"

# 2. Scaffold a new project
tyc init hello && cd hello
# → typhon.toml, src/main.ty, tests/

# 3. Edit src/main.ty
# 4. Check + build + run
tyc fmt src/        # format in place
tyc check src/      # parse + resolve + type-check, no output artifacts
tyc build           # full pipeline → build/main.py + build/*.py.map
python build/main.py
```

A canonical `src/main.ty`:

```python
import sys

def main() -> None:
    let name: str = sys.argv[1] if len(sys.argv) > 1 else "world"
    print(f"Hello, {name}")

if __name__ == "__main__":
    main()
```

The emitted `build/main.py` is byte-similar (formatting aside). **Production never installs anything Typhon-specific** — only a small generated `typhon_runtime.py` when you actually use `Result`, `go`, or `lazy let`.

---

## Type system at depth

### Primitives and widening

| Type | Notes |
|---|---|
| `int` | Arbitrary precision (matches Python). |
| `float` | 64-bit IEEE 754. |
| `bool` | **Not** a subtype of `int` for the checker. |
| `str`, `bytes` | Identical to Python. |
| `None` | Inhabitant of unit; only assignable where `T?` allows. |

`int` widens to `float` (`let y: float = 3` ✅). `float` does **not** narrow to `int` — write `int(x)` / `round(x)`.

### Collections

Element types are required:

```python
let xs: list = [1, 2, 3]         # ❌ implicit Any element
let xs: list[int] = [1, 2, 3]    # ✅
let cs: dict[str, int] = {"a": 1}
let pts: tuple[float, float] = (1.0, 2.0)
let nums: tuple[float, ...] = (1.0, 2.0, 3.0)
```

`dict.get(k)` returns `V?`, not `V`. Either narrow or use `d[k]` (typed `V`, may raise `KeyError`).

### `T?` and flow narrowing

```python
def find_user(id: int) -> str?: ...

let raw: str? = find_user(1)
if raw is None:
    return
greet(raw)                       # ✅ raw narrowed to str

guard r = find_user(1) else: return   # equivalent, prettier
greet(r)
```

`isinstance(x, T)` also narrows. Internally `T?` is `Nullable[T]`; emission is `T | None`.

### Generics — PEP 695 only

```python
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]

class Box[T]:
    value: T

impl[T] Box[T]:
    def map[U](f: Callable[[T], U]) -> Box[U]:
        return Box(value=f(value))

type Vec[T] = list[T]            # transparent alias
```

Inference is bidirectional and recursive; `pair(1, "two")` for `pair[T](a: T, b: T)` widens `T` to `int | str`. **Never import `TypeVar` from `typing`** — that path is rejected.

Generics erase at emit time (PEP 695 syntax is preserved on the 3.13+ default).

### Interfaces (structural)

```python
interface Drawable:
    def draw() -> None
    def width() -> float

class Button:
    label: str

impl Button:
    def draw() -> None: print(label)
    def width() -> float: return 10.0

def render(d: Drawable) -> None:
    d.draw()

render(Button(label="x"))        # ✅ structurally matches
```

Emits as `class Drawable(Protocol): ...`. **`isinstance(x, Drawable)` is rejected** (`tyc::interface_isinstance`) because Python's `@runtime_checkable` only checks attribute *presence*, not signatures. Refactor to a sealed union or write an explicit predicate.

### Sealed unions

```python
type Shape = Circle | Rectangle | Triangle

class Circle:
    radius: float
class Rectangle:
    width: float
    height: float
class Triangle:
    base: float
    height: float

def area(s: Shape) -> float:
    match s:
        case Circle(radius):       return 3.14159 * radius * radius
        case Rectangle(w, h):      return w * h
        case Triangle(b, h):       return 0.5 * b * h
```

The match is statically verified exhaustive. Add `Square` to the alias → every match becomes `tyc::non_exhaustive_match` until handled. Use `case _:` only when you genuinely want a catch-all — it disables exhaustiveness for the rest of the union.

`type X = A | B` without classes is just a Python alias; the *sealed* part is the rule that nothing outside the source file can extend `X` — that's checked at the type level, not at runtime.

### `class` vs `model`

| `class` | `model` |
|---|---|
| Emits `@dataclass(slots=True)` | Emits `class X(BaseModel)` with `model_config = ConfigDict(extra="forbid")` |
| For internal types | For data crossing trust boundaries (HTTP, JSON, env, queues) |
| No runtime validation | Runtime validation via Pydantic |
| Add `frozen` modifier for `frozen=True` | Pydantic's `frozen=True` is *faux* immutability — only blocks field reassignment, not nested mutation |

Override `extra` globally with `[emit] model-extra = "allow" | "ignore"`. Do not write `__init__` — the constructor is generated.

### `impl` and `extend`

- `impl ClassName:` attaches methods to a class declared in the same project. Multiple `impl` blocks for the same class are merged at desugar; they can live in different files.
- `extend ClassName:` is `impl`'s twin for cross-module method addition. Same merge semantics.
- `extend BUILTIN:` (`str`, `list`, `int`, `dict`, …) extracts each method to a module-level free function `__typhon_ext_<TYPE>__<METHOD>`, and rewrites `x.method(...)` to `__typhon_ext_<TYPE>__method(x, ...)` **whenever the receiver `x` is statically annotated as that builtin**. No monkey-patching — un-annotated receivers still raise `AttributeError` at runtime.

### `unsafe:` boundary

```python
def parse() -> int:
    unsafe:
        let v = mystery_lib.get_int()    # would be tyc::implicit_any outside
        let checked: int = int(v)        # re-assert before crossing out
    return checked
```

Inside `unsafe:`, expressions that would otherwise infer `Any` bind freely. Values acquire a hidden `Unsafe[T]` marker that cannot flow into a concrete `T` context outside the block. Block lowers to `if True:` for scope preservation; the checker tracks an `unsafe_depth` counter.

For long-lived dependencies, write a `.dty` stub instead.

---

## Error handling with `Result[T, E]`

`Result[T, E]` is a sealed sum with `Ok(value: T)` and `Err(error: E)`. Emits as frozen dataclasses in a generated `typhon_runtime.py` — no PyPI dep.

```python
def parse_port(raw: str) -> Result[int, str]:
    if not raw.isdigit():
        return Err(f"not a number: {raw}")
    let n: int = int(raw)
    if n < 1 or n > 65535:
        return Err(f"out of range: {n}")
    return Ok(n)
```

### `?` propagation

```python
def parse_addr(host: str, raw: str) -> Result[tuple[str, int], str]:
    let port: int = parse_port(raw)?     # unwrap Ok, short-circuit Err
    return Ok((host, port))
```

`?` is **not** `try/except`. It desugars to:

```python
_tmp_0 = parse_port(raw)
if isinstance(_tmp_0, Err):
    return _tmp_0
port: int = _tmp_0.value
```

Stack traces stay clean. The checker enforces:

- `?` only appears inside a function whose return type is a compatible `Result`.
- Error types must match (or unify under generics). Mismatches → `tyc::result_error_mismatch`.

### `with`-chains

For 3+ chained Results:

```python
def make_report(uid: int) -> Result[Report, AppError]:
    with user   = db.find(uid)?,
         perms  = check(user)?,
         report = build(user, perms)?:
        return Ok(report)
    else err:
        log.warn(err)
        return Err(err)
```

The `else err:` block is optional — without it, the first `Err` short-circuits via the enclosing function (which must return a compatible `Result`).

### Bridging exceptions

Wrap library boundaries in a small `try` shim:

```python
import json

def load(path: str) -> Result[dict[str, str], str]:
    try:
        with open(path) as f:
            return Ok(json.load(f))
    except FileNotFoundError:
        return Err(f"not found: {path}")
    except json.JSONDecodeError as e:
        return Err(f"invalid JSON: {e}")
```

After the shim, downstream code uses `?` and `with`-chains without ever writing `try`.

---

## Async and concurrency

### Explicit `async`, not inferred

- A sync function calling an `async` one without `await` is a **hard error** (`tyc::missing_await`).
- An `async` function with no `await` is a **warning** (`tyc::async_without_await`).

### `gather:` — parallel awaits

```python
async def load(uid: int) -> Dashboard:
    gather:
        user   = fetch_user(uid)
        posts  = fetch_posts(uid)
        notifs = fetch_notifs(uid)
    return Dashboard(user=user, posts=posts, notifs=notifs)
```

Lowers to `asyncio.TaskGroup` (cancel-on-failure). Bindings inside the `gather:` block are an intentional exception to Rule 2 — they don't need `let`/`mut` because the keyword itself introduces them as immutable single-assignment names (the desugarer wraps the whole block as a fresh scope). For best-effort semantics where each binding becomes `T | Exception`:

```python
gather(strategy="best-effort"):
    user = fetch_user(uid)
    posts = fetch_posts(uid)
```

Lowers to `asyncio.gather(..., return_exceptions=True)`.

### Automatic `gather` (opt-in)

`[strictness] auto-gather = true` rewrites straight-line runs of independent `name = await callee(...)` into a `TaskGroup`, **but only when every callee is a same-module `async def` carrying `@gatherable`** and the LHS bindings don't alias. Imported async callees are left untouched so you cannot surprise upstream callers.

The `@gatherable` decorator is the gate — without it, auto-gather is a no-op for that callee:

```python
@gatherable                          # opts this function in to auto-gather
async def fetch_user(uid: int) -> User: ...

@gatherable
async def fetch_posts(uid: int) -> list[Post]: ...

async def load(uid: int) -> Dashboard:
    let user = await fetch_user(uid)     # rewritten into a TaskGroup …
    let posts = await fetch_posts(uid)   # … because both callees are @gatherable
    return Dashboard(user=user, posts=posts)
```

When a run of 2+ adjacent independent awaits would have been gathered but at least one callee lacks `@gatherable`, `tyc build` surfaces a `tyc::auto_gather_missed` advice-level diagnostic naming the missing callee. (Only fires with `[strictness] auto-gather = true`; the nudge is silent when you haven't opted in.) Imported async callees are deliberately excluded — you can't decorate code you don't own — so the diagnostic only flags missed opportunities you could actually fix.

### `go` — fire-and-forget

```python
async def signup(email: str) -> User:
    let user: User = await create(email)
    go send_welcome(user)            # registered with strong ref
    return user
```

Or capture the handle:

```python
go send_welcome(user) -> task
await task                          # later
```

`go` lowers through `typhon_runtime.tasks.spawn`, **never** to a bare `asyncio.create_task` — Python's event loop holds only weak refs, so fire-and-forget can be GC'd mid-flight. The runtime registry holds strong refs and clears entries from a done-callback.

### Free-threaded mode

`[python] free-threaded = true` (requires 3.13t / 3.14t):

- `go` on CPU-bound functions lowers to `ThreadPoolExecutor.submit`.
- The analyser may parallelise pure-function comprehensions via `ThreadPoolExecutor.map`, gated by `[strictness] auto-parallel` and `[strictness] parallel-min-size` (default 64).
- Every parallel block runtime-checks `sys._is_gil_enabled()` and falls back to sequential if a GIL build is detected.

Default off until 3.14 is the default Python.

---

## `let`, `mut`, and what immutability means

`let`/`mut` govern **binding immutability**, not deep value immutability — same as Rust's `let`/`let mut` or TypeScript's `const`/`let`. `let u: User` cannot be reassigned, but `u.name = "x"` is still legal if `User` has a mutable `name` field.

For deep immutability on instances, use `class P frozen:` (emits `frozen=True` on the underlying dataclass / Pydantic config). Note: dataclass `frozen=True` only blocks field reassignment — nested mutable containers can still be mutated. Use `tuple` / `frozenset` inside frozen classes for stronger guarantees.

Parallelisation passes refuse to touch any binding captured as `mut` by a spawned task without explicit sync.

Top-level module bindings default to `let` unless declared `mut`. Inside functions, the keyword is always explicit.

---

## Lazy loading

```python
lazy import np = numpy           # ✅ deferred via bespoke `__TyphonLazy_np_` proxy class
lazy from numpy import array     # ❌ rejected at parse time (PEP 690)
```

`lazy from ... import` defeats deferral (it eagerly touches attributes on the source module) and is a hard parse error. Redirect to `lazy import` + dotted access.

Other lazy forms:

| Form | Lowers to |
|---|---|
| `lazy let CFG: Config = load()` (module-level) | sentinel-cached `lazy_let(lambda: load())` in `typhon_runtime` (thread-safe, one-shot) |
| `lazy let cfg: Config = load()` (class body) | `@cached_property` (per-instance is the intended scope) |
| `def primes(n: int) -> lazy[list[int]]:` | generator function, not materialised list |

Module-level lazy bindings use the runtime helper rather than `functools.cached_property` because the latter is instance-scoped, race-prone, and writable after first eval.

---

## Compile-time evaluation (`comptime`)

`comptime` bindings are evaluated **at build time** in a sandboxed interpreter; results are inlined as literals.

```python
comptime let PORT: int = int(env("PORT", "8080"))
comptime let DB_URL: str = env("DATABASE_URL")   # build fails if unset
comptime let IS_PROD: bool = env("BUILD_TAG", "dev") == "prod"
comptime let TAGS: list[str] = ["alpha", "beta"].split(",") if False else ["alpha", "beta"]
comptime let HOST: str = env("HOST", "localhost").lower()

comptime def feature(name: str) -> bool:
    return env("FEATURE_" + name.upper(), "0") == "1"

comptime let SHIPS_AUTH: bool = feature("auth")
```

Declare required env vars in `typhon.toml`:

```toml
[env]
required = ["DATABASE_URL"]
```

The sandbox is intentionally tight, but covers a useful surface. Today it supports: integer / float / string / boolean literals, container literals (`[1, 2, 3]`, `{"a": 1}`, `(1, "x")`, including empty containers and the trailing-comma single-element tuple form), basic arithmetic (`+ - * / //` and the comparable comparison ops), boolean ops (`and` / `or`), ternaries (`x if cond else y`), `env("NAME")` / `env("NAME", "default")`, the `int()` / `str()` / `float()` / `len()` casts, a small pure-only set of string methods (`upper`, `lower`, `strip`, `lstrip`, `rstrip`, `replace`, `startswith`, `endswith`, `split`), and calls to user-defined `comptime def` functions. Anything else — I/O, subprocess, network, `random` / `time`, arbitrary imports, mutation — is permanently out of scope.

Emitted Python sees only the inlined literal:

```python
PORT: int = 8080
DB_URL: str = "postgresql://..."
```

---

## Purity, `@pure`, `@memo`

A function is **inferable as pure** only if all six conditions hold:

1. Synchronous (no coroutines/generators).
2. Hashable parameters (primitives, frozen dataclasses, tuples thereof).
3. No I/O in transitive call graph (`open`, `socket`, `subprocess`, `print`, logger, DB drivers). `unsafe` + stubbed calls count as impure unless the stub is `@pure`.
4. No non-determinism (`time.*`, `random.*`, `secrets.*`, `uuid.*`, `os.urandom`).
5. No reads/writes of mutable module-level state. `comptime let` reads are fine.
6. No exceptions raised — pure functions express failure via `Result[T, E]`.

```python
@pure                            # asserts purity; compiler enforces all 6 conditions
def normalise(s: str) -> str:
    return s.strip().lower()

@memo                            # implies pure-eligibility, inserts @functools.cache
def fib(n: int) -> int:
    if n < 2: return n
    return fib(n - 1) + fib(n - 2)

@pure(memo=True)                 # combined form
def hash_pw(salt: str, pw: str) -> str: ...
```

Bounded caches: `@memo(max=128)` → `@functools.lru_cache(maxsize=128)`.

Project-wide:

```toml
[strictness]
auto-memoise = true              # off by default — caches extend lifetimes
pgo-memoise = true               # also opt-in; reads typhon-profile.json
pgo-min-calls = 100              # threshold; default 100
```

Manually marking a function `@pure` that fails any of the six conditions is a hard error (`tyc::impure_pure_fn`).

---

## Readability features

### Pipes — left-to-right composition

```python
let cleaned: str = "  Hello, World!  " |> str.strip() |> str.lower() |> str.replace(",", "")
```

`a |> f(arg)` is exactly `f(a, arg)`. Left-associative, fills *first* positional slot. No partial application, no curry.

### Guards

```python
def shipping(weight: float?) -> float:
    guard w = weight else:
        return 0.0
    return w * 1.25
```

Sugar for early-return on falsy/`None` plus narrowing. The `else:` block must return / raise / otherwise exit the enclosing function.

Chain guards naturally:

```python
guard t = token else: return "anon"
guard u = user_id else: return "anon"
return f"({t}, {u})"
```

---

## Python interop and `.dty` stubs

`.dty` is the Typhon stub format — strictly typed in the Typhon dialect (`T?`, `Result`, sealed unions, interfaces, `unsafe`). The compiler emits a PEP 561 `.pyi` companion so mypy / pyright / Pyrefly / `ty` understand it too.

A `.pyi` consumed *by* Typhon is treated as an `unsafe` boundary unless overridden by an authored `.dty`.

```python
# stubs/redis.dty
class Redis:
    host: str
    port: int

impl Redis:
    def get(key: str) -> str?
    def set(key: str, value: str) -> bool
    def delete(*keys: str) -> int
```

`tyc check --stubs` runs a Typhon port of mypy's `stubtest`: it diffs each `.dty`'s surface API against the runtime symbols of the module it describes and emits `tyc::stub_mismatch` for missing-in-impl / missing-in-stub / signature-mismatch findings. Runtime-introspection (full stubtest-proper) is a follow-up.

---

## `typhon.toml` reference

Default scaffold (`tyc init`):

```toml
[project]
name = "myapp"
version = "0.1.0"
src = "src"
out = "build"

[python]
target = "3.13"                  # **required: 3.13+ only**. Valid: "3.13" / "3.13t" / "3.14" / "3.14t". Older values are rejected at config load.
free-threaded = false            # requires 3.13t/3.14t; off by default

[emit]
class-default = "dataclass"      # or "pydantic"
format = true                    # post-process through ruff format
# model-extra is on the roadmap; today Pydantic emissions are always extra="forbid"
# pyi-stubs is always on — every .dty emits a .pyi

[strictness]
no-implicit-any = true
unused-import = "error"          # or "warn" | "off"
exhaustive-match = "error"
methods-in-class-body = "warn"   # or "error" (break CI) | "off"
auto-memoise = false             # opt-in; inserts @functools.cache on inferred pure fns
auto-gather = false              # opt-in; folds independent awaits into TaskGroup (needs @gatherable)
auto-parallel = false            # opt-in; pure list comprehensions → thread-pool map
parallel-min-size = 64
pgo-memoise = false              # opt-in; promotes hot pure fns from typhon-profile.json
pgo-min-calls = 100

[env]
required = ["DATABASE_URL"]      # comptime env() lookups that must resolve at build

[dependencies]
requests = ">=2.31"
rich = "*"                       # bare name → any version

[dev-dependencies]
pytest = "8.2"                   # bare version → ==8.2
```

Notes on always-on behaviour (no longer toggles):

- **PEP 561 `.pyi` stubs are always emitted** alongside every `.dty`.
- **Pydantic emissions are always `extra="forbid"`** — `[emit] model-extra` is on the roadmap, not wired today.

`auto-gather` independence rules:

- Every callee must be a same-module `async def` carrying `@gatherable`.
- LHS bindings must not alias.
- The statements must form a straight-line block.

Externally-imported async callees and undecorated locals are deliberately left alone so flipping the flag doesn't surprise callers that aren't ready for concurrency.

---

## `tyc` subcommand reference

See `docs/cli.md` for the full surface. The most-used commands:

| Command | What it runs | When |
|---|---|---|
| `tyc check src/` | parse → resolve → type → analyse (no emit) | CI; daily editing |
| `tyc build` | full pipeline through emit + ruff format | local run; produces `build/*.py` + `build/*.py.map` |
| `tyc fmt src/` | parse + pretty-print | pre-commit |
| `tyc lsp` | LSP on stdio (diagnostics, hover, go-to-def, member completions via venv introspection, "Remove unused import") | editor |
| `tyc init NAME` | scaffold `typhon.toml`, `src/`, `tests/` | new project |
| `tyc trace traceback.txt` | map Python frames back to `.ty` via `.py.map` | debugging emitted code |
| `tyc profile` | instrument top-level fns with call-count + wall-clock; writes `typhon-profile.json` on interpreter exit | feeds `pgo-memoise` |
| `tyc migrate src/app.py` | typed Python → Typhon: rewrites `Optional[T]`/`T \| None` → `T?`, adds `let`/`mut` to module-level annotated assigns, drops `@dataclass` decorators + import | `--check` for CI |
| `tyc ty` | builds, then runs Astral's `ty` checker over emitted Python | second-opinion type-checking; needs `pip install ty` |
| `tyc repl` | interactive evaluator; compiles each block through the full pipeline | quick experiments; `:quit` / `:reset` / `:show` |
| `tyc debug` | builds + execs `python -m pdb build/main.py` | step through emitted code; pair with `tyc trace` |
| `tyc add` / `tyc remove` / `tyc sync` | manage `[dependencies]` / `[dev-dependencies]`, shell to `uv` | package management |

Notable flags:

- `tyc check --stubs` — also diff every `.dty` against the runtime module it describes.
- `tyc ty --watch` / `tyc ty --out DIR` / `tyc ty -- --strict`
- `tyc repl --load src/lib.ty` / `tyc repl --python python3.13`
- `tyc debug --entry api.py --debugger pudb`
- `tyc add --dev pytest@8.2` / `tyc add --no-sync` / `tyc sync --dry-run`

`tyc repl` quirks: each prompt re-executes the entire accumulated session (pure-scratch semantics, side effects fire once per prompt), multi-line blocks end on the first blank line, no readline/arrow-key support yet. Bare single-line expressions auto-print their `repr(...)` — `>>> 1 + 1` prints `2` — matching the universal REPL convention.

`tyc debug` is a v1 wrapper — frames surface as `build/*.py` paths. Pair with `tyc trace` to remap captured tracebacks back to `.ty`. A Typhon-native source-mapping debugger is a Phase-5 item.

---

## Compiler architecture

The whole pipeline lives in `tyc/crates/`, backed by a Salsa incremental DB.

```
.ty / .dty
    │
    ▼ tyc-syntax       lexer + parser (vendored Ruff fork — see tyc/vendor/)
    │                  let/mut soft-keywords, Mutability AST field
    ▼ tyc-db           Salsa queries: preprocessed_text, module_decl_names, resolved_module
    ▼ tyc-resolve      name resolution + scope construction; enforces let/mut; declaration sites
    ▼ tyc-types        nominal types + non-null narrowing + structural conformance + bidirectional generic inference
    ▼ tyc-analyse      purity (6 conditions), async checks, comptime sandbox, auto-gather data-flow
    ▼ tyc-desugar      Typhon AST → Python AST (merge impl/extend, insert self, expand `?`, with-chains, gather:, go, pipes, lazy let)
    ▼ tyc-emit         Python codegen + `.py.map` v2 (per-statement out_line → ty_line)
    ▼ tyc-format       formatter (`tyc fmt`) — Typhon-aware printer wrapped in `ruff format`
    ▼
    .py + .py.map (+ generated typhon_runtime.py if used)
```

- **`tyc-diagnostics`** uses miette for the human-friendly format you see.
- **`tyc-lsp`** is a `tower-lsp-server` backend reusing the same Salsa DB.
- **`tyc/`** is the CLI binary that wires it all together with clap v4.
- **`tyc/vendor/`** holds the Ruff fork — `ruff_text_size`, `ruff_source_file`, `ruff_python_trivia`, `ruff_python_ast` (with the `Mutability` extension on assignment nodes), `ruff_python_parser`. See `tyc/vendor/README.md` for the fork rationale (it's complete; `rustpython-parser` migration is finished).

When investigating a bug, the rule of thumb is:

| Symptom | Crate to start in |
|---|---|
| Parse error / wrong AST | `tyc/crates/tyc-syntax` (and vendored `ruff_python_parser`) |
| Wrong scope / unknown-name / let-reassignment confusion | `tyc-resolve` |
| Wrong type inference / nullable handling / generic binding | `tyc-types` |
| Wrong purity verdict / wrong async warning / wrong comptime result | `tyc-analyse` |
| Wrong lowering (e.g. `?` produced unexpected code) | `tyc-desugar` |
| Wrong Python output / source-map wrong line | `tyc-emit` |
| Diagnostic text / span wrong | `tyc-diagnostics` (and the call site that emitted it) |
| LSP hover / go-to-def / completion misbehaving | `tyc-lsp` |
| CLI flag, exit code, watch loop | `tyc/crates/tyc` |

---

## Diagnostics catalog

The recurring diagnostic codes and what they actually mean. All are documented in source under `tyc-diagnostics`; this is the field guide.

| Code | Meaning | Fix |
|---|---|---|
| `tyc::missing_return_type` | Function has no `-> T` | Add an explicit return type — `-> None` if it returns nothing |
| `tyc::implicit_any` | RHS infers to `Any` outside `unsafe` | Annotate, wrap in `unsafe:`, or stub the source via `.dty` |
| `tyc::missing_binding_kind` | Local `=` without `let`/`mut` | Add `let` (default) or `mut` (if rebound) |
| `tyc::immutable_assign` | Reassigning a `let` binding | Change to `mut`, or extract a new `let` |
| `tyc::nullable_use` | Passing `T?` where `T` required | Narrow with `is None` / `guard` / early-return |
| `tyc::missing_await` | Sync context calling `async def` | Add `await` and make caller `async` |
| `tyc::async_without_await` (warn) | `async def` with no `await` inside | Drop `async` or await something |
| `tyc::manual_init` | `class` defines `__init__` | Remove it — constructor is generated |
| `tyc::frozen_assign` | Writing a field on a `frozen` class | Build a new instance |
| `tyc::non_exhaustive_match` | `match` on a sealed union misses a variant | Add the missing `case` or use `case _:` |
| `tyc::invalid_question_op` | `?` inside a non-`Result` function | Change the signature or `match` explicitly |
| `tyc::result_error_mismatch` | `?` returns an `Err[E1]` into `Result[T, E2]` | Convert at the boundary |
| `tyc::impure_pure_fn` | `@pure` function fails one of the 6 conditions | Refactor or drop `@pure` |
| `tyc::interface_isinstance` | `isinstance(x, SomeInterface)` | Use static narrowing or refactor to a sealed union |
| `tyc::stub_mismatch` | `.dty` vs `.py` drift detected by `tyc check --stubs` | Update the stub or implementation |
| `tyc::unused_import` | Severity controlled by `[strictness] unused-import` | Remove the import (LSP "Remove unused import" code-action exists) |
| `tyc::method_in_class_body` (warn by default) | A `def` inside `class Name:` instead of `impl Name:` (Rule 4) | Move into an `impl Name:` block. Severity controlled by `[strictness] methods-in-class-body` (`warn` / `error` / `off`). |
| `tyc::auto_gather_missed` (advice) | Adjacent awaits look gather-able but a callee lacks `@gatherable` | Decorate the named callee. Fires from `tyc build` only when `[strictness] auto-gather = true`. |

When in doubt about a diagnostic, `rg "TYC_CODE_NAME" tyc/crates` — every code is registered once in source.

---

## Common pitfalls (the ones every newcomer hits)

1. **Forgetting `-> None`.** Sync functions returning nothing still need the annotation.
2. **Writing `x = 1` at function scope.** Locals require `let` or `mut`. (Module top-level is fine — defaults to `let`.)
3. **Calling `find_user(1)` and passing the result somewhere expecting `str`.** It's `str?`. Narrow first.
4. **Putting `def display(self) -> str` inside `class`.** Move to `impl ClassName:` and drop `self`.
5. **Writing `__init__`.** Don't. Use field defaults or a free function.
6. **`from typing import TypeVar`.** Use PEP 695: `def f[T](xs: list[T]) -> T?:`.
7. **`isinstance(x, MyInterface)`.** Rejected — use static narrowing or a sealed union.
8. **`asyncio.create_task(...)` for fire-and-forget.** Use `go f(x)`; the runtime registry holds a strong ref.
9. **`lazy from numpy import array`.** Rejected. Use `lazy import np = numpy` + `np.array(...)`.
10. **`comptime let NOW: float = time.time()`.** Sandbox forbids `time.*`. Compute at runtime with `lazy let`.
11. **Returning early from a `with`-chain without an `else err:`.** Fine — but only if the enclosing function returns a compatible `Result`.
12. **`dict.get(k)` typed as `V`.** It's `V?`. Either narrow or use `d[k]`.
13. **Empty list with no annotation.** `let xs: list = []` is `tyc::implicit_any`. Write `list[int]` or similar.
14. **Putting blocking I/O inside `async def`.** Typhon doesn't catch this (Python-wide hazard). Use `aiofiles` or `asyncio.to_thread(...)`.
15. **Expecting `bool` to be `int`.** The checker treats them as distinct. Cast explicitly with `int(b)` / `bool(n)`.

---

## Recipes — minimum-viable patterns for common tasks

### Add a runtime dependency

```bash
tyc add requests              # rewrites typhon.toml [dependencies], runs `uv sync`
tyc add --dev pytest@8.2      # dev dep
tyc add --no-sync foo         # batch edits; finish with `tyc sync`
```

### Migrate a typed-Python file

```bash
tyc migrate --check src/app.py    # preview to stdout
tyc migrate src/app.py            # write src/app.ty alongside
tyc check src/                    # fix the diagnostics that remain
```

`tyc migrate` handles the mechanical rewrites; it cannot infer `let`/`mut` inside function bodies (those are added as `let` by default and need manual review for accumulators / counters).

### Debug an emitted-Python traceback back to `.ty`

```bash
python build/main.py 2>err.log
tyc trace err.log                 # remaps frames via build/*.py.map
```

### Run Astral's `ty` over your emitted Python

```bash
pip install ty                    # or: uv tool install ty
tyc ty                            # one-shot
tyc ty --watch                    # re-runs on .ty/.dty change
tyc ty -- --strict                # forward flags to `ty check`
```

### Wire up VS Code

`editors/vscode/` ships a reference extension that runs `tyc lsp` on stdio. See `editors/vscode/README.md`. Any LSP-aware editor can wire `tyc lsp` directly.

### Set up CI

`tyc check` is the CI-recommended command — runs everything up to the analyser without emitting `.py`. Failure cases:

- Any `tyc::*` diagnostic at `"error"` severity.
- Required env var missing (`comptime let` fails when `DATABASE_URL` etc. is unset, if listed in `[env] required`).
- `tyc check --stubs` drift if you ship `.dty` stubs.

Optional second-opinion gate: `tyc ty` after `tyc check`.

---

## Authoring Typhon code as Claude

When you edit `.ty` files in this repo or a downstream project:

1. **Read the relevant guide first** (`docs/guides/`) — every feature has a worked example and its emitted Python listed there. Cross-reference before guessing syntax.
2. **Annotate everything.** If `tyc check` would flag it, write the annotation. There is no implicit `Any` fallback.
3. **Reach for `let` before `mut`.** Only switch to `mut` when you actually need to rebind.
4. **Prefer `Result[T, E]` over `try/except`** anywhere errors are expected (parsing, lookups, validation). Use `try` only at the boundary into untyped libraries.
5. **Prefer sealed unions over inheritance** for closed sets of variants. They give you exhaustive `match`; subclassing doesn't.
6. **Methods go in `impl` blocks**, never inside the `class`. No `self` parameter — the desugarer inserts it.
7. **`extend` for cross-module method addition**, `extend BUILTIN:` for static-only built-in extensions.
8. **`gather:` only for genuinely independent awaits.** If one depends on another's value, leave them sequential.
9. **`go` for fire-and-forget**, never `asyncio.create_task` directly.
10. **`lazy import name = module`** for expensive optional deps; never `lazy from ... import ...`.
11. **`comptime let` for build-time constants** (especially required env vars).
12. **`@pure` only when the six conditions hold.** Mark `@memo` separately or use `@pure(memo=True)`. Never silently rely on `auto-memoise` for code others read.
13. **`unsafe:` is a *lexical* region.** Re-assert types at the boundary, don't smuggle `Unsafe[T]` outward.
14. **After significant edits, run `tyc fmt src/ && tyc check src/`** and read the diagnostics. The checker is the source of truth.
15. **Read the emitted Python** for any non-trivial feature you haven't seen lower before (`tyc build` then look at `build/*.py`). The lowering is the spec.

When you edit the Rust compiler:

1. **Each diagnostic is registered once** in `tyc-diagnostics`. Search for the code to find every site that emits it.
2. **Salsa queries** in `tyc-db` are the cache boundary; if a value should be incrementally tracked, it goes through a query.
3. **The vendored Ruff fork** under `tyc/vendor/` is on a clean branch and tracks upstream loosely; do not edit it without a clear note in `tyc/vendor/README.md`.
4. **Run `cargo test --workspace`** before pushing; the LSP tests use `tower-lsp-server`'s harness and the parser tests round-trip a corpus.

---

## Further reading inside this repo

- **`docs/long-term-plan.md`** — the canonical design doc (everything narrower is excerpted from here).
- **`docs/architecture.md`** — pipeline + crate-by-crate breakdown.
- **`docs/prior-art.md`** — TypeScript, rust-analyzer, ty, Pyrefly, oxc, Ruff influence.
- **`docs/risks.md`** — what we expect to bite us.
- **`docs/roadmap.md`** — phased delivery; Phase 0–3 complete, Phase 4+ in progress.
- **`docs/follow-ups-2026-05-17.md`** — tracked follow-ups.
- **`docs/ty-integration.md`** — how `tyc ty` cooperates with Astral's checker.
- **`docs/performance-baseline.md`** — measured numbers we don't want to regress.
- **`tyc/vendor/README.md`** — Ruff fork rationale.
- **`editors/vscode/README.md`** — reference VS Code extension.

For deeper reference material specific to this skill, see the sibling files:

- **[REFERENCE.md](REFERENCE.md)** — every syntactic form, side by side with its emitted Python.
- **[CLI.md](CLI.md)** — verbose subcommand cheat sheet with flags and exit codes.
- **[PITFALLS.md](PITFALLS.md)** — extended pitfalls catalogue, ranked by frequency.
