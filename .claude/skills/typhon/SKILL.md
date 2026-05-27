---
name: typhon
description: Write, check, build, debug, and migrate code in the Typhon language — a statically-typed, stricter superset of Python that compiles to clean CPython 3.13+ via the `tyc` binary. Use this skill whenever you are editing `.ty` / `.dty` / `typhon.toml` files, invoking any `tyc` subcommand (`build`, `check`, `fmt`, `lsp`, `init`, `run`, `repl`, `debug`, `trace`, `profile`, `explain`, `cheatsheet`, `stubtest`, `migrate`, `ty`, `add`, `remove`, `sync`), translating Python to Typhon, debugging Typhon-specific diagnostics, working on the Rust compiler crates under `tyc/crates/`, or answering any question about the language, the compiler pipeline, the in-process VM, the generated `typhon_runtime`, or the project's docs. Triggers include: file extensions `.ty` / `.dty` / `.py.map` / `typhon.toml`, the words "Typhon" / "tyc" / "let binding" / "mut binding" / "freeze let" / "newtype" / "pub" / "pub *" / "Result[T, E]" / "sealed union" / "interface" / "impl block" / "extend block" / "gather:" / "go f(x)" / "comptime" / "@gatherable" / "@pure" / "@memo" / "T?" / "plain class" / "class!" / "model class" / "lazy import" / "lazy let" / "unsafe:" / "guard" / "|>" / "typhon_runtime" / "tyc-vm" / "tyc-syntax" / "tyc-resolve" / "tyc-types" / "tyc-analyse" / "tyc-desugar" / "tyc-emit" / "tyc-format" / "tyc-db" / "tyc-diagnostics" / "tyc-lsp", and any error code matching `tyc::...`.
---

# Typhon — Language, Compiler, Runtime, and VM Skill

Typhon is **a statically-typed, stricter superset of Python that emits clean CPython 3.13+** with zero runtime dependency on the toolchain. The compiler, language server, formatter, debugger wrapper, REPL, and tree-walking interpreter are all a single Rust binary called `tyc`. Every `.ty` file emits valid, idiomatic `.py`. Not all `.py` is valid Typhon.

**Current release: v0.9.0** (2026-05-27). The language is **additive across the v0.3.0 → v0.9.0 line** — every previously-accepted program continues to type-check identically. v0.9.0 is the stress-test cleanup release closing 32 findings from a v0.8.1 stress sweep: the VM is now usable as the daily-driver runner (`Result` combinators, `open()` write modes, class patterns on built-ins, `frozenset` keys, deep `freeze let`, comptime inlining, `lazy import`, `class!` exception fields, `dataclasses.field(default_factory=...)`, `collections.deque` / `heapq` / `contextlib` / `pydantic` shims, multi-file projects, `@property` / `super()` / `@contextmanager` all work under `tyc run`); the type checker plugs silent-correctness gaps in Sequence covariance, variant-to-parametric-union flow, `while True:` reachability, post-loop narrowing, `assert` narrowing, `*args` annotation policy, `extend list[T]` dispatch, exhaustive match on `T?`, `with`-chain error mismatch, and the `comptime let T: type` alias. v0.8.0 introduced one runtime behaviour change carried into v0.9.0: the VM now uses arbitrary-precision integers (`num_bigint::BigInt`), so programs that relied on silent i64 wrap-around compute mathematically-correct results. Headline frontier work (full HKT unification, general inter-procedural field-init audit, preprocess-line-number remapping for impl-sealed-union diagnostics) is tracked in `TYPE_SYSTEM_FRONTIER.md`.

LLMs have no prior knowledge of Typhon. This skill is the field reference. **Trust the docs and the compiler over assumptions from Python or any superset.** When this skill and a doc disagree, the doc wins. When the docs and the compiler disagree, the compiler wins — verify with `tyc check`.

The canonical sources are:

- **`README.md`** — pitch, release table, workspace layout, top-level project status.
- **`CHANGELOG.md`** — every release back to v0.1.0. v0.3.0–v0.9.0 are the live window.
- **`docs/long-term-plan.md`** — source of truth for design decisions.
- **`docs/language.md`** — the type system, error handling, async, `let`/`mut`, comptime.
- **`docs/cheatsheet.md`** — 30-second syntax refresher (also `tyc cheatsheet`).
- **`docs/cli.md`** — every `tyc` subcommand.
- **`docs/configuration.md`** — every key in `typhon.toml`.
- **`docs/vm.md`** — the in-process tree-walking VM (default execution mode for `tyc run`).
- **`docs/architecture.md`** — pipeline + crate layout.
- **`docs/diagnostics/*.md`** — one page per `tyc::` code; also surfaced via `tyc explain <code>`.
- **`docs/guides/01..10-*.md`** — the teaching surface; read in order the first time.
- **`docs/install.md`** — pre-built binaries (Linux x86_64/aarch64, macOS Apple Silicon/Intel, Windows x86_64) plus source build.
- **`docs/ty-integration.md`** — how `tyc ty` cooperates with Astral's checker.
- **`docs/performance-baseline.md`** — measured numbers we don't want to regress.
- **`docs/roadmap.md`** / **`docs/risks.md`** / **`docs/prior-art.md`** / **`docs/findings.md`** — context.
- **`examples/01..68-*/`** — 68 stdlib-only exercises.
- **`examples/apps/01..15-*/`** — 15 production-shaped multi-file projects (event-sourced banking, distributed KV, mini-compiler, search engine, GraphQL server, game ECS, trading engine, ML orchestrator, web crawler, task scheduler, real-time game server, static site generator, vector DB, API gateway, stream processor).
- **`tyc/vendor/README.md`** — Ruff fork rationale.
- **`editors/vscode/README.md`** — reference VS Code extension (v0.2.0).

This skill ships sibling files for the long-tail detail:

- **[REFERENCE.md](REFERENCE.md)** — every Typhon syntactic form, side-by-side with its emitted Python.
- **[CLI.md](CLI.md)** — verbose subcommand reference with every flag, exit code, and example.
- **[PITFALLS.md](PITFALLS.md)** — extended catalogue of the surprises every newcomer hits.
- **[DIAGNOSTICS.md](DIAGNOSTICS.md)** — exhaustive `tyc::` code reference, grouped by category.
- **[COOKBOOK.md](COOKBOOK.md)** — canonical patterns extracted from `examples/`.
- **[RUNTIME.md](RUNTIME.md)** — the generated `typhon_runtime` package and the in-process VM.
- **[PACKAGING.md](PACKAGING.md)** — multi-file projects, `__init__.ty`, `pub *` aggregation.

---

## 1. When to invoke this skill

Trigger automatically when the session involves any of:

1. **Authoring or editing `.ty` source.** Re-read the relevant guide section before writing significant code — Typhon's surface looks Python-like but at least eight rules diverge silently (Section 4).
2. **Editing `.dty` stubs.** These are Typhon's source of truth for third-party Python APIs; they emit `.pyi` for interop. Drift is caught by `tyc check --stubs` and `tyc stubtest`.
3. **Editing `typhon.toml`.** Every strictness flag and emit knob has subtle defaults — see [§13 configuration reference](#13-typhontoml-reference).
4. **Working inside the Rust compiler** (`tyc/crates/`). The pipeline is `syntax → resolve → types → analyse → desugar → emit → format`, backed by a Salsa DB. See [§16 compiler architecture](#16-compiler-architecture).
5. **Migrating `.py` → `.ty`.** Use `tyc migrate` first — it rewrites `Optional[T]` → `T?`, `Generic[T]` → PEP 695, drops `@dataclass`, rewrites `NewType` and `Protocol`. Then resolve diagnostics manually.
6. **Debugging a `tyc::...` diagnostic.** The [DIAGNOSTICS.md](DIAGNOSTICS.md) catalog is the fastest lookup; `tyc explain <code>` works offline.
7. **Onboarding someone to Typhon.** Walk them through the [§3 cheat sheet](#3-cheat-sheet) first, then the guides.

---

## 2. Hello, Typhon — the shortest realistic flow

```bash
# 1a. Install a pre-built binary (macOS / Linux):
curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh

# 1b. Windows (PowerShell):
iwr -useb https://raw.githubusercontent.com/codehalwell/typhon/main/install.ps1 | iex

# 1c. Or build from source (from repo root):
cd tyc && cargo build --release && cd ..
alias tyc="$PWD/tyc/target/release/tyc"

# 2. Scaffold a new project
tyc init hello && cd hello
# → typhon.toml, src/main.ty, tests/

# 3. Edit src/main.ty (see below)
# 4. Iterate
tyc fmt src/         # whitespace-normalise + ruff format wrap
tyc check src/       # parse + resolve + type-check, no output artifacts
tyc run              # default: execute via the in-process tree-walking VM
tyc build            # full pipeline → build/main.py + build/.sourcemaps/*.py.map
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

The emitted `build/main.py` is byte-similar (formatting aside). **Production never installs anything Typhon-specific** — only a small generated `typhon_runtime/` package when you actually use `Result`, `go`, `lazy let`, `freeze let`, or auto-parallel comprehensions.

---

## 3. Cheat sheet

The 30-second mental model. Every later section in this skill is detail under one of these bullets.

| Topic | Typhon | Emitted Python |
|---|---|---|
| Local binding | `let x: int = 1` / `mut x: int = 1` | `x: int = 1` |
| Module binding | `X: int = 1` (implicit `let`) or `mut X: int = 1` | `X: int = 1` |
| Declare-then-assign (v0.7.0) | `let loaded: Cfg` then assign on every non-diverging arm | `loaded: Cfg` then `loaded = …` |
| Typed tuple unpack (v0.3.1) | `let (a: int, b: str) = pair()` | hidden temp + per-element typed assigns |
| Deep-immutable binding | `freeze let CFG = {"port": 8080, "hosts": ["a", "b"]}` | `CFG = __typhon_freeze__({...})` (deep-freezes value) |
| Public name | `pub let API_VERSION: str = "v1"` / `pub class Foo: ...` | synthesised `__all__ = [...]` at top of module |
| Package re-export (v0.7.0) | `pub *` in `__init__.ty` | aggregates sibling modules + sub-packages |
| Nominal alias | `newtype UserId = int` | `UserId = NewType("UserId", int)` — asymmetric |
| Same-newtype arithmetic (v0.7.0) | `let x: Index = a + b` | preserved across `+ - * // % **`; `/` still widens to `float` |
| Nullable | `name: str?` | `name: str \| None` |
| Optional default | `name: str? = None` (no auto-default) | `name: str \| None = None` |
| Class | `class User: id: int` | `@dataclass(slots=True) class User: id: int` |
| Pydantic model | `model ApiUser: id: int` | `class ApiUser(BaseModel): model_config = ConfigDict(extra="forbid"); id: int` |
| Frozen class | `class P frozen: x: float` | `@dataclass(slots=True, frozen=True)` |
| Plain class (no decorator) | `plain class Bag: items: list[str]` | bare `class Bag:` (no `@dataclass`, no synthesised `__init__`) |
| Raw class (framework base) | `class! M(nn.Module): layer: nn.Linear` | bare `class M(nn.Module):` with synthesised `__init__` calling `super().__init__()` |
| Methods | `impl User: def display(self) -> str: ...` | merged into the class body |
| Extend foreign class | `extend User: def x(self) -> int: ...` | merged at desugar |
| Extend built-in | `extend str: def slug(self) -> str: ...` | extracted to `__typhon_ext_str__slug` free fn + call-site rewrites |
| Sealed union | `type Shape = Circle \| Rectangle` | `Shape = Circle \| Rectangle` (just a type alias) |
| `impl` on sealed union (v0.6.0) | `impl Shape: def area(self) -> float: ...` | distributes the method to every variant |
| Exhaustive match | `match s: case Circle(r): ...` (no `_` needed) | vanilla Python `match` |
| Result type | `Result[T, E]`, `Ok(v)`, `Err(e)` | generated `typhon_runtime.Ok/Err` dataclasses |
| Result combinators (v0.6.0) | `r.map(f) / r.map_err(g) / r.and_then(h) / r.or_else(k)` | method calls on the runtime classes |
| Error propagation | `let n: int = f()?` | inline `isinstance(_t, Err): return _t; n = _t.value` |
| Result chain | `with a = f()?, b = g()?: ...  else err: ...` | sequenced if-isinstance ladder |
| Generic fn | `def first[T](xs: list[T]) -> T?:` | same (PEP 695) |
| Generic class | `class Box[T]: value: T` / `impl[T] Box[T]:` | preserved (PEP 695) |
| HKT scaffold (v0.5.0) | `class Functor[F[_]]:` | parsed; staged for unification |
| Interface | `interface Drawable: def draw(self) -> None` | `class Drawable(Protocol): ...` |
| Pipe | `a \|> f() \|> g(arg)` | `g(f(a), arg)` |
| Guard | `guard x = expr else: return ...` | `if expr is None: return ...; x = expr` |
| Parallel awaits | `gather: a = f(); b = g()` | `async with asyncio.TaskGroup() as _tg: ...` |
| Best-effort gather | `gather(strategy="best-effort"):` | `asyncio.gather(..., return_exceptions=True)` |
| Spawn | `go f(x)` / `go f(x) -> task` | `typhon_runtime.tasks.spawn(...)` (strong-ref registry) |
| `await` middleware-callable (v0.7.0) | `let r: Resp = await next(req)` where `next: Callable[[Req], Awaitable[Resp]]` | unwraps to `Resp` |
| Lazy module | `lazy import np = numpy` | bespoke `__TyphonLazy_np_` proxy with double-checked locking |
| Lazy module-level let | `lazy let CFG: Config = load()` | sentinel-cached `lazy_let(lambda: load())` |
| Lazy class-level let | `lazy let cfg: Config = ...` inside class | `@cached_property` |
| Comptime constant | `comptime let PORT: int = int(env("PORT", "8080"))` | inlined literal at build time |
| Comptime type-value (v0.5.0) | `comptime let T: type = int` | inlined; supports 8 primitive heads |
| Pure assertion | `@pure def f(...) -> T:` | nothing emitted unless `@memo` too |
| Memoised | `@memo def fib(n: int) -> int:` | `@functools.cache` |
| Auto-gather opt-in | `@gatherable async def fetch_x(...) -> X:` | enables auto-gather rewrite |
| Unsafe boundary | `unsafe: let x = mystery_lib()` | `if True:` (scope-preserving) |
| `with` as-target typing (v0.7.0) | `with conn() as c:` types `c` from `__enter__` / `@contextmanager` factory's yield | works for async too |

Run `tyc cheatsheet` for the same table at the terminal.

---

## 4. The eight rules every Typhon program follows

These are the rules behind every "but the same code works in Python" surprise. The first five are the foundational ones; the last three landed in v0.3.0 → v0.7.0.

### Rule 1 — Every parameter and return type is annotated

```python
def add(a: int, b: int) -> int:    # ✅
    return a + b

def add(a, b):                     # ❌ tyc::missing_annotation
    return a + b
```

`-> None` is mandatory for sync functions that return nothing. There is no inference fallback. This is `[strictness] no-implicit-any = true` (default on, the implicit-any escape is parsed for forward-compat but currently has no effect — the check is always on).

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
- `for` / `with` / `case` / `except` targets are bindings, not assignments — they don't take `let`/`mut` and don't collide with outer `let` bindings of the same name (v0.6.0+).

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

Narrowing forms the checker understands: `is None`, `is not None`, `isinstance(x, T)`, `guard`, early-return `if x is None: return`, exhaustive `match`, **ternary** `body if test else orelse` (v0.7.0), the truthy-falsy union picks of `or` / `and` (Python semantics). De Morgan narrowing (`if not (A or B): return` refines both operands afterwards) lands in v0.4.0.

`T?` emits as `T | None`.

### Rule 4 — Methods live in `impl`, not in `class`

```python
class User:
    id: int
    name: str

impl User:
    def display(self) -> str:    # explicit `self`; use `self.NAME`
        return f"{self.name} (#{self.id})"
```

Writing `__init__` inside `class` is rejected (`tyc::manual_init`) — the constructor is generated. Methods in the class body fire `tyc::method_in_class_body` (severity `warn` by default; configurable via `[strictness] methods-in-class-body`). The bare-identifier sugar where `name` resolves to `self.name` is **not** implemented; write `self.NAME` explicitly.

`impl` blocks can span multiple files via `impl ClassName:` (same project) and `extend ClassName:` (cross-module, identical merge semantics). `impl` on a **sealed-union alias** distributes the methods to every variant automatically (v0.6.0):

```python
type Event = TaskStarted | TaskFinished | TaskFailed

impl Event:
    def is_terminal(self) -> bool:
        match self:
            case TaskStarted(_): return False
            case TaskFinished(_) | TaskFailed(_): return True
```

### Rule 5 — `Any` only enters through `unsafe:` or `.dty` stubs

```python
import messy

let data = messy.fetch()         # ❌ tyc::missing_annotation / implicit Any

unsafe:
    let data = messy.fetch()     # ✅ inside the region
let parsed: dict[str, int] = ... # re-assert at the boundary
```

`unsafe:` is a *lexical region*, not a per-value annotation. Values inside acquire a hidden `Unsafe[T]` marker that cannot cross out into a concrete-typed context without a re-assertion (annotation, narrowing, or cast). The block lowers to `if True:` so scope rules are unchanged. Smuggling an `Unsafe[T]` outward fires `tyc::unsafe_value_leak`.

For long-lived dependencies, write a `.dty` stub instead.

### Rule 6 — Exhaustive `match` on sealed unions

```python
type Shape = Circle | Rectangle | Triangle

def area(s: Shape) -> float:
    match s:
        case Circle(radius):       return 3.14159 * radius * radius
        case Rectangle(w, h):      return w * h
        # ❌ tyc::non_exhaustive_match — Triangle uncovered
```

Severity controlled by `[strictness] exhaustive-match` (default `"error"`). Use `case _:` only when a catch-all is genuinely the intent (it disables exhaustiveness for the remaining variants). Keyword patterns (`case TaskStarted(task_id=tid):`) also satisfy exhaustiveness (v0.6.0).

### Rule 7 — Errors flow as `Result[T, E]`, not exceptions

```python
def parse_port(raw: str) -> Result[int, str]:
    if not raw.isdigit():
        return Err(f"not a number: {raw}")
    let n: int = int(raw)
    if n < 1 or n > 65535:
        return Err(f"out of range: {n}")
    return Ok(n)
```

`?` propagates `Err` cleanly inside any `Result`-returning function. Bridging to exceptions happens at library boundaries via a small `try` shim. See [§9 error handling](#9-error-handling).

### Rule 8 — Definite assignment for declare-only `let` (v0.7.0)

```python
def parse(raw: str) -> Result[Cfg, str]:
    let loaded: Cfg                          # declare-only; OK in v0.7.0+
    match _load(raw):
        case Ok(v):  loaded = v
        case Err(e): return Err(e)
    return Ok(loaded)                        # ✅ both arms either assign or diverge
```

The resolver tracks each uninitialised `let` declaration's span; the first subsequent assignment **is** the initialiser. The second assignment to a `let` still fires `tyc::immutable_assign`. `mut NAME: T` without initialiser is also accepted (any number of subsequent assignments legal). Reads on a path where the binding hasn't been assigned fire `tyc::use_of_uninitialised`; `if`/`elif`/`else` and `match` arms each count as separate first-assignment paths; `return` / `raise` / `continue` / `break` arms are excluded from the intersection.

---

## 5. Type system

### 5.1 Primitives and widening

| Type | Notes |
|---|---|
| `int` | Arbitrary precision (matches CPython). VM today uses `i64`. |
| `float` | 64-bit IEEE 754. |
| `bool` | **Subtype of `int`** (v0.4.0). `let x: int = True` type-checks; `let b: bool = 1` does not — assignability is one-way. |
| `str`, `bytes` | Identical to Python. |
| `None` | Inhabitant of unit; only assignable where `T?` allows. |

`int` widens to `float` (`let y: float = 3` ✅). `float` does **not** narrow to `int` — write `int(x)` / `round(x)`. `bool` flows into `int` for arithmetic: `1 + True` types as `int`; `-True` types as `int`.

### 5.2 Collections

Element types are required:

```python
let xs: list = [1, 2, 3]         # ❌ implicit Any element / missing_annotation
let xs: list[int] = [1, 2, 3]    # ✅
let cs: dict[str, int] = {"a": 1}
let pts: tuple[float, float] = (1.0, 2.0)
let nums: tuple[float, ...] = (1.0, 2.0, 3.0)
```

`dict.get(k)` returns `V?`, not `V`. Either narrow or use `d[k]` (typed `V`, may raise `KeyError`).

**Fixed-arity tuple covariance** (v0.4.0): `tuple[int, int]` widens both slots to `float` at the assignment site. Mutable containers (`list`, `dict`, `set`) remain invariant.

TypedDict-style dict literals match field shapes (v0.3.0): `let alice: User = {"id": 1, "name": "Alice"}` matches a `model User` declaration field-by-field.

Set difference: `set - set` and `frozenset - frozenset` type-check (v0.5.2).

### 5.3 `T?` and flow narrowing

```python
def find_user(id: int) -> str?: ...

let raw: str? = find_user(1)
if raw is None:
    return
greet(raw)                       # ✅ raw narrowed to str

guard r = find_user(1) else: return   # equivalent, prettier
greet(r)
```

`isinstance(x, T)` also narrows. Internally `T?` is `Nullable[T]`; emission is `T | None`. `while`-condition narrowing applies test-implied narrowings to the body (v0.3.0). Ternary narrowing (v0.7.0): `let r: int = x if x is not None else 0` types `x` as the non-null form inside the `x` arm.

### 5.4 Generics — PEP 695 only

```python
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]

class Box[T]:
    value: T

impl[T] Box[T]:
    def map[U](self, f: Callable[[T], U]) -> Box[U]:
        return Box(value=f(self.value))

type Vec[T] = list[T]            # transparent alias
type Pair[A, B] = tuple[A, B]
```

Inference is bidirectional and recursive; `pair(1, "two")` for `pair[T](a: T, b: T)` widens `T` to `int | str`. **Never import `TypeVar` from `typing`** — that path is rejected with `tyc::typevar_import_rejected`. Same for the deprecated capitalised aliases (`List`, `Dict`, `Tuple`, `Set`, `FrozenSet`, `Type`) → `tyc::typing_alias_deprecated`.

**HKT scaffold (v0.5.0):** the parser accepts `F[_]` as a type-constructor parameter (`class Functor[F[_]]:`); full unification is partial. Use it conservatively until the frontier work in `TYPE_SYSTEM_FRONTIER.md` lands.

**Cross-module generic method dispatch propagates class TypeVars (v0.7.0):** `s: Stream[int]; s.map(f)` records `Callable[[int], U]` as the expected parameter and returns `Stream[U]`. Same for field access on a generic — `r: RecordEnv[int]; r.payload` is `int`, not bare `T`.

Generics erase at emit time (PEP 695 syntax is preserved on the 3.13+ default).

### 5.5 Interfaces (structural)

```python
interface Drawable:
    def draw(self) -> None
    def width(self) -> float

class Button:
    label: str

impl Button:
    def draw(self) -> None: print(self.label)
    def width(self) -> float: return float(len(self.label) + 4)

def render(d: Drawable) -> None:
    d.draw()

render(Button(label="x"))        # ✅ structurally matches
```

Emits as `class Drawable(Protocol): ...`. **`isinstance(x, Drawable)` is rejected** (`tyc::interface_isinstance`) because Python's `@runtime_checkable` only checks attribute *presence*, not signatures. Refactor to a sealed union or write an explicit predicate. Interface methods match against methods in the candidate, not against fields of callable type — mixing is rejected (`tyc::interface_not_conforming`).

### 5.6 Sealed unions

```python
type Shape = Circle | Rectangle | Triangle

class Circle:    radius: float
class Rectangle: width: float; height: float
class Triangle:  base: float;  height: float
```

The cheat-sheet form `sealed union Shape: Circle(radius: float); Square(side: float)` is also accepted. Variants can be parametric (`type EventEnvelope[T] = RecordEnv[T] | WatermarkEnv | BarrierEnv` — some refer to `T`, others don't). For nullary variants, write `class Nil frozen: pass` and match with `case Nil():` (two empty parens, not `case Nil(_):`).

The match is statically verified exhaustive. Add `Square` to the alias → every match becomes `tyc::non_exhaustive_match` until handled. Cross-module variant flow works (v0.6.0) — variant `A(...)` flows into an `Event`-typed slot in a consumer module even when the alias is declared in another package.

### 5.7 `class` vs `model` vs `plain class` vs `class!`

| Form | Emits | Use when |
|---|---|---|
| `class Foo:` | `@dataclass(slots=True)` | Plain value type (default). |
| `class Foo frozen:` | `@dataclass(slots=True, frozen=True)` | Immutable value type. Field reassignment → `tyc::frozen_assign`. |
| `model Foo:` | `class Foo(BaseModel):` + `model_config = ConfigDict(extra="forbid")` | Validated input at a system boundary. Pydantic dep required. |
| `interface Foo:` | `class Foo(Protocol):` (bodies `...`) | Structural contract. |
| `plain class Foo:` | bare `class Foo:` (no decorator, no `__init__`) | Metaclass-driven libs, descriptor-based models (Textual, Django ORM, SQLAlchemy declarative). |
| `class! Foo(Base):` | bare `class Foo(Base):` + synthesised `__init__` calling `super().__init__()` and assigning fields | Subclassing a framework base that owns its own `__init__` (torch.nn.Module, custom exceptions). |

#### Mutable-default rewrite

Bare mutable literals (`tags: list[str] = []`) are rewritten at desugar to `dataclasses.field(default_factory=list)`. Applies to `class` and `class … frozen`; **skipped** for `model`, `interface`, and `class!`.

#### Auto-skip for framework bases

A `class Foo(Base):` whose `Base`'s last identifier segment is `Enum`, `IntEnum`, `StrEnum`, `Flag`, `IntFlag`, `ABC`, `ABCMeta`, `Protocol`, `TypedDict`, `NamedTuple`, `BaseModel`, or `App` emits without `@dataclass`. Project bases can be added via `[emit] skip-decoration-bases = ["MyBase", ...]`. Auto-skip drops only the decorator; it does not synthesise `__init__`. Use `class!` when you need both.

#### Field default ordering (v0.7.0)

`@dataclass` rejects a non-default field after a defaulted one (would raise `TypeError` at import). `tyc::field_default_ordering` catches it at check time. Move every non-defaulted field above defaulted ones, or use a factory.

#### `class!` constructor synthesis rules

- No `@dataclass` decorator.
- `__init__` auto-synthesised when the body has no user `def __init__` and ≥1 base is present:
  - Calls `super().__init__()` first (no positional/keyword args — pass them via the body if needed).
  - Assigns each annotated field through `self`, in source order. Non-defaulted fields are positional; defaulted fields follow.
- **Class-level field defaults are stripped from the body** when `__init__` is synthesised (prevents PyTorch-style double evaluation as a class attribute then per-instance).
- Annotations remain visible to type checkers.
- A hand-written `__init__` is preserved verbatim.

#### Inheritance

Single inheritance is the supported form (`class Dog(Animal):`). Subclass constructors accept inherited fields (v0.4.0): `Dog(name="Rex", breed="Husky")` works against `class Dog(Animal):` where `name` lives on `Animal`. Dataclass inheritance ordering rules still apply across the MRO — non-defaults before defaults all the way up.

### 5.8 `impl` and `extend`

- `impl ClassName:` attaches methods to a class declared in the same project. Multiple `impl` blocks for the same class merge at desugar; they can live in different files.
- `impl[T] ClassName[T]:` introduces type parameters scoped over the methods; methods may add their own (`def map[U](...)`).
- `impl OtherType:` on an **alias** of a sealed union distributes the methods to every variant (v0.6.0). Duplicate-method check fires when the same method exists on both `impl Union:` and `impl Variant:`.
- `extend ClassName:` is `impl`'s twin for cross-module method addition. Same merge semantics.
- `extend BUILTIN:` (`str`, `list`, `int`, `dict`, …) extracts each method to a module-level free function `__typhon_ext_<TYPE>__<METHOD>`, and rewrites `x.method(...)` to the free-function call **whenever the receiver `x` is statically annotated as that built-in**. No monkey-patching — un-annotated receivers still raise `AttributeError` at runtime. `extend list[int]:` (parametric target) fires `tyc::extend_builtin` — drop the brackets.

### 5.9 `unsafe:` boundary

```python
def parse() -> int:
    unsafe:
        let v = mystery_lib.get_int()    # would be tyc::missing_annotation outside
        let checked: int = int(v)        # re-assert before crossing out
    return checked
```

Inside `unsafe:`, expressions that would otherwise infer `Any` bind freely. Values acquire a hidden `Unsafe[T]` marker that cannot flow into a concrete `T` context outside the block. Block lowers to `if True:` for scope preservation; the checker tracks an `unsafe_depth` counter. Smuggling `Unsafe[T]` outward fires `tyc::unsafe_value_leak`.

For long-lived dependencies, write a `.dty` stub instead. Common idiom: an `unsafe:` block always ends in `return checked` or `raise RuntimeError("unreachable")` — never let the block be the last thing in a non-`-> None` function.

---

## 6. v0.3.0 — annoyance-killing language features

### `newtype Name = Base` — nominal aliases over primitives

```python
newtype UserId = int
newtype PostId = int

def fetch_user(id: UserId) -> User: ...

let uid: UserId = UserId(42)
fetch_user(uid)                  # ✅
fetch_user(42)                   # ❌ tyc::newtype_violation — wrap as UserId(42)

let raw: int = uid               # ✅ asymmetric: UserId flows freely into int
```

Compiles to a zero-cost `typing.NewType` call. The relationship is asymmetric: `Newtype → Base` is free, `Base → Newtype` requires explicit construction.

**Same-newtype arithmetic preserves the newtype across `+ - * // % **`** (v0.7.0). `LogIndex(a) + LogIndex(b)` is `LogIndex`. `LogIndex(a) + 1` (literal of base) is also `LogIndex`. Two distinct newtypes with the same base (`LogIndex + Term`) fire `tyc::operator_type_mismatch`. `/` always widens to `float` (Python's true division).

Use newtypes for ID kinds, currency tags, internal-vs-external markers — anywhere "an `int` is an `int`" loses you minutes of debugging.

### `freeze let X = expr` — deep-immutable bindings

`let` only locks the binding name; `freeze let` locks the value too:

```python
freeze let CONFIG = {"port": 8080, "hosts": ["a", "b"]}

# CONFIG = {...}                 # ❌ tyc::immutable_assign (binding locked)
# CONFIG["port"] = 9000          # ❌ TypeError at runtime — MappingProxyType
# CONFIG["hosts"].append("c")    # ❌ AttributeError at runtime — tuple, not list
```

Module-level only in v1. Lowers to a `__typhon_freeze__(...)` call against `typhon_runtime.freeze.deep_freeze`, which recursively converts `list → tuple`, `dict → MappingProxyType`, `set → frozenset`, descends into nested values, and raises `TypeError` at startup on anything without a clean immutable equivalent (file handles, sockets, generators, non-frozen dataclasses). Frozen dataclasses pass through unchanged.

Stacks with `pub` (v0.6.0): `pub freeze let X = …` parses.

### `pub` — module visibility marker

```python
pub let API_VERSION: str = "v1"
pub class Client: host: str
pub def connect(host: str) -> Client: ...

let _internal_default_port: int = 8080   # not exported
```

When a module declares at least one `pub` name, desugar synthesises a top-of-file `__all__ = [...]` so `from foo import *`, Sphinx autoapi, IDE re-export filters, and the type checker's re-export inference all see the public surface. A hand-written `__all__` wins.

Stacks with every modifier: `pub frozen class`, `pub model`, `pub let`, `pub mut`, `pub freeze let`, `pub newtype`, `pub interface`, `pub type`, `pub def`, `pub async def`.

### `pub *` — package-level re-export aggregation (v0.7.0)

```python
# src/mypkg/__init__.ty
pub *
```

In an `__init__.ty`, `pub *` aggregates every direct-sibling module's `pub` names and (transitively) every direct sub-package's effective public surface. The desugar pass synthesises `from .sibling import name1, name2, ...; from .subpkg import name3, ...` at the marker and appends every aggregated name to `__all__`.

Colliding sibling names → `tyc::pub_name_collision` (names both modules and the colliding name). `pub *` outside `__init__.ty` is a no-op + `tyc::pub_star_outside_init` (advice).

See [PACKAGING.md](PACKAGING.md) for the full multi-file packaging surface.

### Three new safety/effect diagnostics

- **`tyc::blocking_in_async`** (warn) — direct call to a known-blocking stdlib (`time.sleep`, `requests.{get,post,...}`, `urllib.request.urlopen`, `subprocess.{run,call,check_call,check_output}`, `input`, `socket.recv`) inside `async def`. Suppressed inside `unsafe:`. Wrap in `asyncio.to_thread(...)` or use an async-native client.
- **`tyc::resource_not_managed`** (warn) — bare assignment of `open` / `socket.socket` / `sqlite3.connect` / `tempfile.{NamedTemporaryFile,TemporaryDirectory,TemporaryFile}` not wrapped in `with`. Severity controlled by `[strictness] require-with`. `@contextmanager` / `@asynccontextmanager` factory bodies are exempt (v0.6.0).
- **`tyc::div_by_zero_literal`** — literal divisor zero (`/ 0`, `// 0`, `% 0`, including `-0.0` and unary-negated zero) always raises `ZeroDivisionError`. Pure constant-fold; no flow analysis.

Plus `tyc::unsafe_value_leak`, `tyc::pattern_shadows_outer`, and `tyc::extend_builtin` (all from the same release).

---

## 7. v0.9.0 highlights

The v0.3.0 → v0.9.0 line is **additive**. Every previously-accepted program continues to type-check identically; the one runtime behaviour change is the VM's switch to arbitrary-precision integers in v0.8.0 (programs that relied on silent i64 wrap-around now compute different (correct) results). Highlights since v0.3.0:

### v0.9.0 — stress-test cleanup release

The big v0.8.1 → v0.9.0 sweep. Closes 32 of 36 findings from a v0.8.1 stress sweep. The VM now runs the surface the docs always advertised; the type checker plugs silent correctness gaps in covariance, variant flow, narrowing, and error propagation. See `CHANGELOG.md` for the full list.

**VM coverage** (closing the gap between `tyc run` and `tyc build && python build/main.py`):

- **`Result` combinators** (`.map` / `.map_err` / `.and_then` / `.or_else`) work on `Ok` / `Err` values in the VM via bound `NativeFn` wrappers (v0.9.0). Previously a typecheck-clean program crashed at run-time with `AttributeError: Ok has no attribute 'and_then'`.
- **`open()` write / append / binary modes** (v0.9.0). `open(p, "w")` / `open(p, "a")` / `open(p, "wb")` / `open(p, "r+")` and friends work. `with`-blocks honour `__enter__` / `__exit__`. `json.load` / `json.dump` ride on top.
- **Match against built-in class patterns** (v0.9.0). `match x: case str() as s:` / `case int() as n:` / etc. match; exhaustiveness recognises `case None:` + `case str() as s:` as covering `str?`.
- **`frozenset(...)` hashable as a dict key** (v0.9.0) — new `HashKey::FrozenSet` variant with insertion-order-independent hashing.
- **f-string `_` thousands separator** (v0.9.0) emits the same way `,` does.
- **`bytes` repr matches CPython** (v0.9.0) — `b'hi'` by default, `b"with 'embedded'"` fallback, `\xNN` for non-printable bytes.
- **Native shims for `collections.deque`, `heapq`, `contextlib`, `pydantic`** (v0.9.0). Graph / queue / heap algorithms, `@contextmanager` identity decorators, and `model` class declarations all run cleanly. `deque` rides on `Value::List` via new `popleft` / `appendleft` / `extendleft` / `rotate` list methods. `pydantic.BaseModel` is a placeholder.
- **`@property` / `@classmethod` / `@staticmethod` / `super()`** builtins (v0.9.0) — identity-ish stubs so decorated methods don't crash on import.
- **`lazy import np = numpy`** (v0.9.0) uses the simpler `import M as N` rewrite in VM mode (the descriptor-based proxy class the build path emits has nothing to bind against in a tree-walking VM).
- **Multi-file projects** under both `tyc run` modes (v0.9.0). The VM loads sibling `.ty` modules from the project source root, honours relative imports (`from .repo import x`), and caches each module's bindings as a `Value::Module`. `tyc run --compile` spawns `python -m <pkg>.main` instead of `python build/main.py` so relative imports in the entry point resolve correctly.
- **`dataclasses.field(default_factory=list)` invokes the factory per instance** (v0.9.0). `tags: list[str] = []` no longer shares one list across every instance.
- **`class!` synthesised `__init__` runs** (v0.9.0). `except HttpError as e: print(e.code)` works against `class! HttpError(Exception): code: int; message: str` — the handler binds the user `Instance`, and exception-type matching walks the MRO.
- **`freeze let CFG = {...}` actually freezes** (v0.9.0): list → tuple, dict → mappingproxy-tagged dict, recursive. Mutators on a frozen dict raise the same `TypeError` CPython's `MappingProxy` does.
- **`comptime let X = ...` inlines in the VM** (v0.9.0) via the substitution pass shared with `tyc build`. `comptime let PORT = int(env(...))` no longer crashes with `NameError: env is not defined`.
- **Typed tuple unpack `let (a: int, b: str) = pair()` parses in the VM** (v0.9.0; parity with `tyc check`).

**Type checker** (silent-correctness gaps):

- **Read-view covariance** (v0.9.0). `list[Subclass]` / `tuple[Subclass]` / `set[Subclass]` / `frozenset[Subclass]` flow into `Sequence[Super]` / `Iterable[Super]` / `Iterator[Super]` / `Collection[Super]` / `Container[Super]` / `Reversible[Super]`. Mapping / MutableMapping cover `dict[K, V]` (K invariant, V covariant).
- **Variant → parametric sealed union assignability** (v0.9.0). `Cons[T]` / `Cons` (where `type LL[T] = Cons[T] | Nil`) is assignable into `LL[T]`. Required for recursive ADT walks like `mut cur: LL[T] = self`.
- **`while True:` reachability** (v0.9.0). A loop whose body always returns/raises with no `break` is recognised as exiting; the post-loop point is unreachable and `missing_return` doesn't fire.
- **Post-while-loop narrowing** (v0.9.0). After `while y is None: y = load()` (no `break`), `y` is narrowed to non-None after the loop.
- **`assert x is not None` narrows** (v0.9.0) — the standard Python static-checker idiom works.
- **`*args` / `**kwargs` require annotations** (v0.9.0; Rule 1). Canonical idiom is `*args: object` / `**kwargs: object`.
- **`extend list:` dispatches on `list[T]`-annotated receivers** (v0.9.0). The synthetic `__typhon_builtin_ext_list` class shape is consulted before `attribute_not_found` fires.
- **Exhaustive `match` on `T?` recognises built-in class patterns** (v0.9.0). `case None: ...; case str() as s: ...` against `str?` no longer surfaces `missing_return`.
- **`with`-chain explicit `else err: return Err(err)` validates the error type** (v0.9.0). Previously the check was gated on the synthetic `?`-op temp shape, so a `with`-chain could silently return the wrong error class.
- **`func[T](args)` explicit type instantiation** (v0.9.0) fires a clear check-time error instead of crashing at runtime with `'function' object is not subscriptable`.
- **`comptime let T: type = int`** (v0.9.0) lowers to a PEP 695 `type T = int` alias so `T` is substitutable wherever a type is expected. `tyc check` runs the substitution before parsing the resolved module so check, build, and VM all see the same shape.
- **`freeze let X = <expr>` validates freezability at check time** (v0.9.0). New `tyc::freeze_not_freezable` fires when the RHS constructs a non-`frozen` user class, instead of letting the failure surface as a runtime `TypeError` at first import.
- **`pub *` name collisions surface in `tyc check`** (v0.9.0). The detection logic from `tyc build` is exposed as `detect_pub_star_diagnostics` and called from the check command, so CI catches collisions before they reach build.

**Diagnostics polish** (v0.9.0):

- **`interface_not_conforming` arity message** reads "got N non-self parameter(s), expected M" instead of the ambiguous "arity N; expected M".
- **`invalid_question_op` help text** mentions both the Result-return cause AND the comprehension carve-out.
- **Sealed-union impl distribution dedupes** diagnostics by `(code, rendered message)` so a 10-variant union no longer reports 10 identical errors.
- **`class_attr_shadows_slot` no longer false-positives** on classes whose only annotated defaults are mutable literals (`list[str] = []` etc.). Those become `default_factory` per-instance fields.
- **`MissingAnnotation` text** drops the double-backtick wrapping (was `` `parameter `x`` ``).

**Docs** (v0.9.0):

- Cheat sheet documents `class X frozen(Base):` (the modifier comes BETWEEN the class name and the base list, NOT after `(Base)`) and the `*args: object` / `**kwargs: object` idiom for genuinely variadic functions.

**Known limitations carried forward**:

- Preprocess line-number leakage (B15) — diagnostics still report preprocessed-buffer line numbers for `impl Alias:` distribution over sealed unions. The dedupe pass cuts the *count* of noise diagnostics but each surviving diagnostic still points at a synthetic line index past EOF of the original source.

### v0.8.1 — bugfix point release

- **`tyc::attribute_not_found` no longer fires on venv-introspected third-party classes** (v0.8.1). The v0.8.0 firing-site widening trusted shapes built by `inspect.signature(Cls)` to be method-complete, so `obj.method(...)` against any third-party class with a known `__init__` flagged the call as missing the attribute (`uvicorn.Server.serve(...)`, `httpx.AsyncClient.aclose(...)`, `fastapi.Request.body(...)`). `InterfaceShape` now carries a `partial` flag set on every venv-derived shape; `class_hierarchy_fully_known` returns `false` whenever any class in the chain is partial. Strictly a narrowing of the diagnostic; no language, runtime, or stdlib changes.

### v0.8.0 — stress-test sweep

- **`tyc::attribute_not_found` fires on class instances and generic classes** (v0.8.0) — not just `TypeVar`-bounded parameters. Foreign / venv-introspected classes carry a `partial` shape marker and stay lenient (so `uvicorn.Server.serve(...)`, `httpx.AsyncClient.aclose(...)`, `fastapi.Request.body(...)` don't false-positive). Skipped in `unsafe:` and on dunder / underscore names.
- **Interface parameter type conformance** (v0.8.0) — `interface_missing_members` compares param types position-by-position (contravariant) in addition to arity.
- **`Type::LitStr(String)` — string-literal singleton types** (v0.8.0) — `type Color = "red" | "green" | "blue"` and `Literal["a", "b"]` produce `LitStr` slots. Bidirectional inference widens string literals to `LitStr` only when the expected type carries one.
- **`?` propagation inside `with`-chains** (v0.8.0) — `result_error_mismatch` fires when the implicit return form of `with x = f()?: …` routes a mismatching error type.
- **`tyc::pattern_shadows_outer`** (v0.8.0) — fires when a `match` capture binds a name already in the outer scope.
- **`newtype Foo = "literal"` is rejected** (v0.8.0) — new `tyc::newtype_invalid_base` diagnostic.
- **`field_default_ordering` skips `ClassVar` fields** (v0.8.0).
- **Exhaustive-match-with-guards no longer fires `missing_return`** (v0.8.0) when every variant has at least one (possibly-guarded) case.
- **Function parameter rebinding requires `mut`** (v0.8.0) — matching the `let`/`mut` rule everywhere else.
- **VM: arbitrary-precision integers** (v0.8.0) — `Value::Int` is now `num_bigint::BigInt`. `2 ** 100` and `fib(99)` no longer overflow. **Behaviour change.**
- **VM: insertion-ordered dicts** (v0.8.0) — `RcDict` is now `indexmap::IndexMap`. Same `.ty` no longer prints dicts in different orders under `tyc run` vs `tyc build && python build/main.py`.
- **VM: f-string format flags fully wired** (v0.8.0) — zero-pad, alternate-form, `[fill]align`, sign, width, comma, precision, type all match CPython.
- **VM: mapping match patterns + sequence-with-star patterns** (v0.8.0) — `case {"k": v}`, `case {…, **rest}`, `case [x, *rest, y]`.
- **VM: recursion limit raised to 1000** (v0.8.0) to match CPython.
- **VM: larger native stdlib** (v0.8.0) — `re`, `typing`, `collections` (`OrderedDict`, `defaultdict`, `Counter`, `namedtuple`), `functools` (`lru_cache`, `cache`, `cached_property`, `reduce`, `partial`), `itertools` (`chain`, `count`, `cycle`, `accumulate`, `combinations`, `permutations`, `product`, `islice`, `takewhile`, `dropwhile`, `groupby`), `dataclasses`, `pathlib`.
- **VM: subclass constructors inherit fields** (v0.8.0) — `class Dog(Animal): breed: str` accepts `Dog(name=…, age=…, breed=…)` under `tyc run`.
- **VM: `yield` / `async def` emit a clear `NotImplementedError`** (v0.8.0) pointing at `tyc build && python` as the fallback (instead of crashing the interpreter).
- **Parser scaffolds for advertised forms** (v0.8.0): HKT `class Functor[F[_]]:`, `impl[T] SealedUnionAlias[T]:` distributing methods across every variant, generic-plus-frozen `class X[T] frozen:`, `async def` in `interface` bodies auto-completing the body, outer-annotation tuple unpack (`let (a, b): tuple[int, str] = …`).
- **Synthetic preprocess lines no longer leak into diagnostics** (v0.8.0) — `SanitisedDiagnostic` wraps every emitted diagnostic.
- **Diagnostic hints polished** (v0.8.0): multi-line `|>` chains, `freeze let` at non-module scope, `wrong_arg_count` kw-only rephrasing, collection variance suggesting `Sequence[Animal]` / `Mapping[K, V]` / `frozenset[T]`, dict-to-model mismatch pointing at the constructor form.
- **New lint warnings** (v0.8.0): `tyc::empty_collection_no_annotation`, `tyc::typing_alias_in_annotation`, `tyc::contains_secret_literal`.
- **CLI polish** (v0.8.0): `tyc check lib.dty` accepts a single `.dty` file directly; `tyc run --compile` rejects single-file inputs up-front; `tyc migrate` strips trivial `__init__` methods.
- **Default change** (v0.8.0): `unused_import` is now `warn` (was `error`). Restore via `[strictness] unused-import = "error"`.

### v0.7.1 — LSP bugfix

- **LSP semantic-tokens positions** now align with the original `.ty` source instead of the preprocessed Python view. Pure bugfix; no language or runtime changes.

### Earlier highlights since v0.3.0

### Type-system relaxations

- **`bool ⊆ int`** (v0.4.0) — `let x: int = True`, `1 + True`, `-True` all check.
- **De Morgan narrowing** (v0.4.0) — `if not (A or B): return` narrows both operands afterwards.
- **Fixed-arity tuple covariance** (v0.4.0) — `tuple[int, int]` widens both slots to `float`.
- **Subclass constructors accept inherited fields** (v0.4.0).
- **Dotted-attribute annotations** resolve to foreign class shapes (v0.4.0).
- **Variance table** expanded with `AsyncContextManager`, `KeysView`, `ValuesView`, `ItemsView`, `Type`, `Counter` (v0.5.0).
- **`set - set` / `frozenset - frozenset`** type-check (v0.5.2).
- **`Ok` / `Err` Result combinators** as methods (v0.6.0): `.map`, `.map_err`, `.and_then`, `.or_else`.
- **`impl` on a sealed-union alias** distributes to every variant (v0.6.0).
- **Cross-module variant → sealed-union flow** (v0.6.0).
- **Cross-module function signatures preserve param/return types** (v0.6.0).
- **`pub freeze let X = …`** parses (v0.6.0).
- **`pub def` visible to `?` validator** (v0.6.0).
- **For-target doesn't rebind prior `let` bindings** (v0.6.0).
- **Sibling `case` arms: `let` declarations don't shadow each other** (v0.6.0).
- **`pub *` wildcard re-export aggregation** in `__init__.ty` (v0.7.0).
- **Declare-only `let NAME: T`** with definite-assignment analysis (v0.7.0).
- **`with cm() as r:` / `async with cm() as r:`** types `r` from `__enter__` / `@contextmanager`-decorated factories (v0.7.0).
- **`await Callable[..., Awaitable[T]]`** unwraps to `T` (v0.7.0) — canonical async-middleware `let r: Resp = await next(req)` works.
- **Same-newtype arithmetic preserves the newtype** (v0.7.0).
- **Ternary narrowing** (v0.7.0) — `body if test else orelse` narrows like `if`/`else`.
- **Cross-module generic method dispatch** propagates class TypeVars (v0.7.0).
- **`from X import Y`** inside `if`/`for`/`while`/`with`/`try`/`match` arms binds (v0.7.0).
- **Sibling `if`/`elif` branches** don't trip `no_block_shadow` (v0.7.0).
- **Multi-line `go expr(...)`** parses (v0.7.0).

### Diagnostics added

- `tyc::duplicate_method` (v0.3.1).
- `tyc::stdlib_module_shadow` (v0.6.0, refined v0.7.0) — `.ty` file shadowing Python 3.13 stdlib top-level module names.
- `tyc::pub_name_collision` (v0.7.0).
- `tyc::pub_star_outside_init` (v0.7.0, advice).
- `tyc::use_of_uninitialised` (v0.7.0).
- `tyc::field_default_ordering` (v0.7.0).

### CLI additions

- **`tyc run` gates the VM behind a static `tyc check`** (v0.3.1). Set `TYC_SKIP_CHECK=1` to bypass; `--compile` always gates on the full build.
- **`tyc migrate` rewrites `Generic[T]` → PEP 695** (v0.3.1).
- **`tyc migrate @dataclass(frozen=True)` → `class X frozen:`** (v0.5.0).
- **`tyc migrate class X(Protocol[T]):` → `interface X[T]:`** (v0.5.0).
- **`tyc migrate NAME = NewType("NAME", BASE)` → `newtype NAME = BASE`** (v0.5.0).
- **`tyc ty` diagnostic attribution** via `.py.map` (v0.5.0) — rewrites `path.py:LINE:COL` to `.ty` coordinates. `--raw` opts out.
- **`tyc debug` Typhon-aware pdb wrapper** (v0.5.0) — surfaces `[ty] <src>:<line>` after every pause; loads all `.py.map` at startup. `--raw-pdb` opts out.
- **`tyc debug --break ty-file:line`** translates `.ty` coordinates through `.py.map` (v0.5.0).
- **Grouped `tyc check` diagnostics by source file** (v0.3.1) plus per-code summary tally.
- **`tyc explain --list`** prints every diagnostic code (v0.6.0 docs).
- **`.py.map` sidecars move to `<out>/.sourcemaps/`** (v0.6.1); resolvers prefer the new location, legacy adjacent layout still readable.

### "Designed but NOT yet supported" — avoid these forms

- **`lazy let X: T:` colon-block form** does NOT parse. Use `lazy let X: T = expr`.
- **`lazy[T]` return-type form** is designed but unimplemented.
- **Multi-line `|>` pipes require wrapping parens.**
- **`model X frozen:` does NOT parse** — `frozen` is on `class` only.
- **`let`-shadowing is rejected** — use `mut` or a fresh name; never re-bind with `let`.
- **`from typing import TypeVar`** is rejected — use PEP 695.
- **`from typing import List` / `Dict` / `Tuple` etc.** is rejected — use lowercase builtins.
- **Bounded TypeVars** parse; multi-argument constraint solving is partial.
- **PEP 612 `ParamSpec`** is not modelled — annotate decorator layers with `Callable[..., Any]`.
- **Full HKT unification** is in progress; the parser accepts `F[_]` but checker treatment is staged.

---

## 8. `let`, `mut`, and what immutability means

`let`/`mut` govern **binding immutability**, not deep value immutability — same as Rust's `let`/`let mut` or TypeScript's `const`/`let`. `let u: User` cannot be reassigned, but `u.name = "x"` is still legal if `User` has a mutable `name` field.

For deep immutability on instances, use `class P frozen:` (emits `frozen=True` on the underlying dataclass / Pydantic config). Note: dataclass `frozen=True` only blocks field reassignment — nested mutable containers can still be mutated. Use `tuple` / `frozenset` inside frozen classes for stronger guarantees.

For deep immutability on module-level bindings, use `freeze let CONFIG = {...}` (v0.3.0) — the value is recursively frozen via `typhon_runtime.freeze.deep_freeze` at startup.

Parallelisation passes refuse to touch any binding captured as `mut` by a spawned task without explicit sync.

Top-level module bindings default to `let` unless declared `mut`. Inside functions, the keyword is always explicit.

### Typed tuple unpacking (v0.3.1)

```python
let (a: int, b: str) = func(x, y)
let (a: int, b)      = pair()       # mixed; un-annotated leg uses inference
let (xs: list[int], ys: list[int]) = split()  # compound annotations OK
```

Desugars to a hidden `__typhon_unpack_N__` temp plus per-element typed assigns. The top-level-comma split inside the annotation pair survives compound annotations like `list[int]`, `dict[str, int]`, `tuple[float, ...]`.

### Declare-then-assign `let NAME: T` (v0.7.0)

```python
def parse(raw: str) -> Result[Cfg, str]:
    let loaded: Cfg
    match _load(raw):
        case Ok(v):  loaded = v
        case Err(e): return Err(e)
    return Ok(loaded)
```

The resolver tracks each uninitialised `let` declaration's span; the FIRST subsequent assignment silently succeeds (it IS the initialiser). The standard `tyc::immutable_assign` fires on any SECOND assignment. Sibling `match` arms and sibling `if` / `elif` / `else` bodies each count as a separate first-assignment path. `mut NAME: T` without initialiser is also accepted (any number of subsequent assignments legal). Reads on a path that hasn't assigned fire `tyc::use_of_uninitialised` with labels on both use site and declaration.

---

## 9. Error handling with `Result[T, E]`

`Result[T, E]` is a sealed sum with `Ok(value: T)` and `Err(error: E)`. Emits as frozen dataclasses in a generated `typhon_runtime/result.py` — no PyPI dep.

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

- `?` only appears inside a function whose return type is a compatible `Result`. `?` in a non-`Result` fn → `tyc::invalid_question_op`.
- Error types must match (or unify under generics). Mismatches → `tyc::result_error_mismatch`. Convert at the boundary via `match`.
- `?` inside a comprehension is rejected (cannot hoist out of the comp's scope; v0.3.1).
- Inline `?` is supported (v0.3.0): `Ok(add(parse(s)?, parse(t)?))` works.

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

### Combinators (v0.6.0)

`Ok` and `Err` carry `.map`, `.map_err`, `.and_then`, `.or_else` methods. For heterogeneous error pipelines:

```python
let toks: Tokens   = tokenize(src).map_err(_lex_to_pipeline)?
let ast:  Ast      = parse(toks).map_err(_parse_to_pipeline)?
let ty:   TypedAst = check(ast).map_err(_type_to_pipeline)?
```

Semantics: `Ok.map(f)` transforms value, `Ok.map_err(g)` is identity (vice versa for `Err`); `and_then` chains a `Result`-returning op on `Ok`; `or_else` recovers from `Err`.

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

## 10. Async and concurrency

### Explicit `async`, not inferred

- A sync function calling an `async` one without `await` is a **hard error** (`tyc::missing_await`).
- An `async` function with no `await` is a **warning** (`tyc::async_without_await`).
- Direct call to known-blocking stdlib (`time.sleep`, `requests.*`, `subprocess.run`, …) inside `async def` → **`tyc::blocking_in_async`** (warn).
- `loop.run_until_complete(coro())` does NOT fire `tyc::missing_await` (v0.6.0).

### `gather:` — parallel awaits

```python
async def load(uid: int) -> Dashboard:
    gather:
        user   = fetch_user(uid)
        posts  = fetch_posts(uid)
        notifs = fetch_notifs(uid)
    return Dashboard(user=user, posts=posts, notifs=notifs)
```

Lowers to `asyncio.TaskGroup` (cancel-on-failure):

```python
async with asyncio.TaskGroup() as _tg:
    _t_user   = _tg.create_task(fetch_user(uid))
    _t_posts  = _tg.create_task(fetch_posts(uid))
    _t_notifs = _tg.create_task(fetch_notifs(uid))
user   = _t_user.result()
posts  = _t_posts.result()
notifs = _t_notifs.result()
```

Bindings inside the `gather:` block are an intentional exception to Rule 2 — they don't need `let`/`mut` because the keyword itself introduces them as immutable single-assignment names. Dependent bindings (one references an earlier binding) gracefully degrade to sequential `await` in source order.

For best-effort semantics where each binding becomes `T | Exception`:

```python
gather(strategy="best-effort"):
    user = fetch_user(uid)
    posts = fetch_posts(uid)
```

Lowers to `asyncio.gather(..., return_exceptions=True)`.

### Automatic `gather` (opt-in)

`[strictness] auto-gather = true` rewrites straight-line runs of independent `name = await callee(...)` into a `TaskGroup`, **but only when every callee is a same-module `async def` carrying `@gatherable`** and the LHS bindings don't alias. Imported async callees are left untouched so flipping the flag doesn't surprise upstream callers.

```python
@gatherable
async def fetch_user(uid: int) -> User: ...

@gatherable
async def fetch_posts(uid: int) -> list[Post]: ...

async def load(uid: int) -> Dashboard:
    let user  = await fetch_user(uid)     # rewritten into a TaskGroup …
    let posts = await fetch_posts(uid)    # … because both callees are @gatherable
    return Dashboard(user=user, posts=posts)
```

When a run of 2+ adjacent independent awaits would have been gathered but at least one callee lacks `@gatherable`, `tyc build` surfaces a `tyc::auto_gather_missed` advice-level diagnostic naming the missing callee. (Only fires with `[strictness] auto-gather = true`; the nudge is silent otherwise.)

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

Multi-line `go expr(...)` parses (v0.7.0).

### Async-callable awaits (v0.7.0)

```python
async def middleware(next: Callable[[Req], Awaitable[Resp]], req: Req) -> Resp:
    let resp: Resp = await next(req)        # ✅ unwraps Awaitable[Resp] to Resp
    return resp
```

`await` on a `Callable[..., Awaitable[T]]` / `Coroutine[Y, S, T]` call unwraps to `T`. This unblocks canonical async-middleware shapes.

### Free-threaded mode

`[python] free-threaded = true` (requires 3.13t / 3.14t):

- `go` on CPU-bound functions lowers to `ThreadPoolExecutor.submit`.
- The analyser may parallelise pure-function comprehensions via `typhon_runtime.parallel.map_pure(...)`, gated by `[strictness] auto-parallel` and `[strictness] parallel-min-size` (default 64). Set, dict, and list comprehensions are all eligible (v0.5.0).
- Every parallel block runtime-checks `sys._is_gil_enabled()` and falls back to sequential if a GIL build is detected.

Default off until 3.14 is the default Python.

---

## 11. Lazy loading

```python
lazy import np = numpy           # ✅ deferred via bespoke `__TyphonLazy_np_` proxy class
lazy from numpy import array     # ❌ rejected at parse time (PEP 690 reasoning)
```

`lazy from ... import` defeats deferral (it eagerly touches attributes on the source module) and is a hard parse error. Redirect to `lazy import` + dotted access.

Other lazy forms:

| Form | Lowers to |
|---|---|
| `lazy let CFG: Config = load()` (module-level) | sentinel-cached `lazy_let(lambda: load())` in `typhon_runtime` (thread-safe, one-shot) |
| `lazy let cfg: Config = load()` inside class body | `@cached_property` (per-instance is the intended scope) |
| `def primes(n: int) -> lazy[list[int]]:` | designed but **unimplemented today** — use `Iterator[int]` directly |

Module-level lazy bindings use the runtime helper rather than `functools.cached_property` because the latter is instance-scoped, race-prone, and writable after first eval.

**Doc-confirmed non-working forms** (avoid these):
- `lazy let X: T:` colon-block form does NOT parse.
- `lazy[T]` return type form is designed but unimplemented.

---

## 12. Compile-time evaluation (`comptime`)

`comptime` bindings are evaluated **at build time** in a sandboxed interpreter; results are inlined as literals.

```python
comptime let PORT: int = int(env("PORT", "8080"))
comptime let DB_URL: str = env("DATABASE_URL")   # build fails if unset
comptime let IS_PROD: bool = env("BUILD_TAG", "dev") == "prod"
comptime let HOST: str = env("HOST", "localhost").lower()

comptime def feature(name: str) -> bool:
    return env("FEATURE_" + name.upper(), "0") == "1"

comptime let SHIPS_AUTH: bool = feature("auth")

comptime let T: type = int        # v0.5.0 — types-as-comptime-values
```

Declare required env vars in `typhon.toml`:

```toml
[env]
required = ["DATABASE_URL"]
```

### Allowed in the sandbox

- **Statements (in `comptime def` body):** `return`, local bindings (`x = …`, `let x: T = …`, `mut x: T = …`), `if`/`elif`/`else`.
- **Expressions:** literals (`int`, `float`, `str`, `bool`), container literals (`[1, 2]`, `{"a": 1}`, `(1, "x")`, including empty containers and the trailing-comma single-element tuple form), arithmetic (`+ - * / // % **`), comparisons (`== != < <= > >=`), boolean ops (`and or not`), ternaries (`x if cond else y`), `env(name, default?)`, the `int()` / `str()` / `float()` / `len()` casts, a small pure-only set of string methods (`upper`, `lower`, `strip`, `lstrip`, `rstrip`, `replace`, `startswith`, `endswith`, `split`, `join` (v0.3.1)), subscript with Python negative indexing, calls to user-defined `comptime def` functions.
- **Types-as-values (v0.5.0):** `int`, `str`, `bool`, `float`, `bytes`, `None`, `type`, `object`. Stored as `ComptimeValue::Type(...)`.

### Forbidden

- Loops, exceptions (`raise`, `try`/`except`), `with`-blocks, `class` declarations, nested `def` (other than the outer `comptime def`), arbitrary imports.
- I/O, network, subprocess, random, time, uuid, `os.urandom`.
- Free variables (module-level names that aren't parameters or local bindings) — comptime is **hermetic**; pass everything in as arguments.
- `*args`, `**kwargs`, defaults, keyword-only parameters on a `comptime def`.
- Recursion depth capped at **64**.

### Emitted Python

```python
PORT: int = 8080
DB_URL: str = "postgresql://..."
SHIPS_AUTH: bool = True
```

`comptime def` functions are **also preserved** in the emitted `.py` so they remain callable at runtime.

### Secret-shape literal warning

`tyc::contains_secret_literal` (warn) fires when a `comptime let` binding's name matches `*KEY`, `*TOKEN`, `*PASSWORD`, `*SECRET`, `*PASS`, `*PWD` — the build artifact would contain the resolved env-var value as a string literal. Read at runtime via `os.environ[...]` instead.

---

## 13. `typhon.toml` reference

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
class-default = "dataclass"      # or "pydantic". Unknown values → tyc::invalid_config_value
format = true                    # post-process through ruff format
model-extra = "forbid"           # "forbid" | "allow" | "ignore"
skip-decoration-bases = []       # extra base-class names suppressing the auto @dataclass decoration. Matched by last segment.
# pyi-stubs is always on — every .dty emits a .pyi

[strictness]
no-implicit-any = true           # reserved for forward compat; today the check is always on
unused-import = "error"          # or "warn" | "off"
exhaustive-match = "error"
methods-in-class-body = "warn"   # or "error" (break CI) | "off"
require-with = "warn"            # severity for tyc::resource_not_managed
blocking-in-async = "warn"       # severity for tyc::blocking_in_async
stub-check = "error"             # severity for tyc::stub_mismatch
auto-memoise = false             # opt-in; inserts @functools.cache on inferred pure fns
auto-gather = false              # opt-in; folds independent awaits into TaskGroup (needs @gatherable)
auto-parallel = false            # opt-in; pure list/set/dict comprehensions → thread-pool map
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

Notes on always-on behaviour:

- **PEP 561 `.pyi` stubs are always emitted** alongside every `.dty`.
- **Pydantic emissions inject `model_config = ConfigDict(extra=…)`**, controlled by `model-extra`.
- **`tyc::stdlib_module_shadow`** is gated on the presence of `typhon.toml` (standalone-file checks skip it).

`auto-gather` independence rules:

- Every callee must be a same-module `async def` carrying `@gatherable`.
- LHS bindings must not alias.
- The statements must form a straight-line block.

---

## 14. `tyc` subcommand reference

See [CLI.md](CLI.md) and `docs/cli.md` for the full surface. The most-used commands:

| Command | What it runs | When |
|---|---|---|
| `tyc check src/` | parse → resolve → type → analyse (no emit) | CI; daily editing |
| `tyc build` | full pipeline through emit + ruff format; `--check` for dry-run | local run; produces `build/*.py` + `build/.sourcemaps/*.py.map` |
| `tyc fmt src/` | in-process whitespace pass + `ruff format` wrap (when on PATH) | pre-commit |
| `tyc run` | execute a Typhon program in the in-process VM by default; `--compile` (alias `--no-vm`) falls back to build-then-exec for CPython library interop | iterating on pure-Typhon code |
| `tyc lsp` | LSP on stdio (diagnostics, hover, go-to-def, member completions via venv introspection, from-import members from sibling files, "Remove unused import") | editor |
| `tyc init NAME` | scaffold `typhon.toml`, `src/`, `tests/` with a worked `main.ty` (frozen dataclass + `impl` + `Result`/`?`/`match`) | new project |
| `tyc trace traceback.txt` | map Python frames back to `.ty` via `.py.map` v2 | debugging emitted code |
| `tyc profile` | instrument top-level fns with call-count + wall-clock; writes `typhon-profile.json` on interpreter exit | feeds `pgo-memoise` |
| `tyc migrate src/app.py` | typed Python → Typhon: rewrites `Optional[T]`/`T \| None` → `T?`, `Generic[T]` → PEP 695, `NewType` → `newtype`, `Protocol` → `interface`, drops `@dataclass`/`@dataclass(frozen=True)`, adds `let`/`mut` to module-level annotated assigns | `--check` for CI |
| `tyc ty` | builds, then runs Astral's `ty` checker over emitted Python with `.ty` path attribution via `.py.map` (v0.5.0) | second-opinion type-checking; needs `pip install ty` |
| `tyc stubtest` | builds, then runs `python -m mypy.stubtest` against every emitted `.pyi` | runtime probe complementing `tyc check --stubs` |
| `tyc repl` | interactive evaluator; compiles each block through the full pipeline | quick experiments; `:quit` / `:reset` / `:show` |
| `tyc debug` | builds + execs `python -m pdb build/main.py` with a source-mapping wrapper (v0.5.0) that surfaces `[ty]` paths; `--break <ty-file>:<line>` translates `.ty` coordinates through `.py.map` | step through emitted code; pair with `tyc trace` |
| `tyc explain <code>` | prints the catalog entry for a `tyc::` diagnostic (short or fully-qualified code); `tyc explain --list` prints every code | when a diagnostic needs more context |
| `tyc cheatsheet` | prints the 30-second Typhon cheat sheet to stdout | offline syntax refresher |
| `tyc add` / `tyc remove` / `tyc sync` | manage `[dependencies]` / `[dev-dependencies]`, shell to `uv` | package management |

Notable flags:

- `tyc check --stubs` — also diff every `.dty` against the runtime module it describes.
- `tyc build --check` — dry-run, lists every file that would be written without touching disk.
- `tyc build --no-sync` (or `TYC_NO_SYNC=1`) — skip `uv sync` but still merge `pyproject.toml`.
- `tyc run --compile` (alias `--no-vm`) — fall back to build-then-exec when the program imports CPython-only libraries.
- `tyc run --temp` — compile mode only; build into a tempdir deleted on exit.
- `tyc ty --watch` / `tyc ty --out DIR` / `tyc ty -- --strict` / `tyc ty --raw`
- `tyc repl --load src/lib.ty` / `tyc repl --python python3.13`
- `tyc debug --entry api.py --debugger pudb --break src/main.ty:42` / `tyc debug --raw-pdb`
- `tyc add --dev pytest@8.2` / `tyc add --no-sync` / `tyc sync --dry-run`

`tyc repl` quirks: each prompt re-executes the entire accumulated session (pure-scratch semantics, side effects fire once per prompt), multi-line blocks end on the first blank line, no readline/arrow-key support yet. Bare single-line expressions auto-print their `repr(...)` — `>>> 1 + 1` prints `2`.

`tyc debug` v0.5.0 wraps `pdb.Pdb` so the **entire debugger UI reads `.ty` coordinates** — `list`, `where`, stack frames, and the prompt all display `.ty` paths and source slices. `--raw-pdb` opts out. `--break <ty-file>:<line>` translates Typhon source locations through `.py.map` and forwards them to the chosen debugger as `-c "break …"`.

`tyc run` defaults to the in-process `tyc-vm` tree-walking interpreter — no `.py` written, no CPython spawn. As of v0.3.1 it gates on `tyc check` first (set `TYC_SKIP_CHECK=1` to bypass). See [RUNTIME.md](RUNTIME.md) for the supported feature surface. Programs that import CPython-only libraries fall back via `tyc run --compile`.

---

## 15. Python interop and `.dty` stubs

`.dty` is the Typhon stub format — strictly typed in the Typhon dialect (`T?`, `Result`, sealed unions, interfaces, `unsafe`). The compiler emits a PEP 561 `.pyi` companion so mypy / pyright / Pyrefly / `ty` understand it too.

A `.pyi` consumed *by* Typhon is treated as an `unsafe` boundary unless overridden by an authored `.dty`.

```python
# src/stubs/redis.dty
class Redis:
    host: str
    port: int

impl Redis:
    def get(self, key: str) -> str?
    def set(self, key: str, value: str) -> bool
    def delete(self, *keys: str) -> int
```

`tyc check --stubs` runs a Typhon port of mypy's `stubtest`: it diffs each `.dty`'s surface API against the runtime symbols of the module it describes and emits `tyc::stub_mismatch` for missing-in-impl / missing-in-stub / signature-mismatch findings. `tyc stubtest` runs the real `python -m mypy.stubtest` against every emitted `.pyi` (catches dynamically-created members the AST can't see — `__init_subclass__`, metaclass-driven member registration, Pydantic auto-generated fields).

**Cross-module shape extraction consumes both `.ty` and `.dty` on equal footing.** When both define the same name, stubs win.

---

## 16. Compiler architecture

The whole pipeline lives in `tyc/crates/`, backed by a Salsa incremental DB.

```
.ty / .dty
    │
    ▼ tyc-syntax       lexer + parser (vendored Ruff fork — see tyc/vendor/)
    │                  let/mut soft-keywords with Mutability AST field
    │                  preprocess.rs expands T?, ?, |>, gather:, go, lazy, with-chains
    ▼ tyc-db           Salsa queries: preprocessed_text/full, module_decl_names,
    │                  resolved_module, check_diagnostics, module_shapes_query
    ▼ tyc-resolve      name resolution + scope construction; enforces let/mut;
    │                  declaration sites; per-arm uninit tracking for v0.7.0 DA
    ▼ tyc-types        nominal types + non-null narrowing + structural conformance +
    │                  bidirectional generic inference; HKT scaffold (TypeConstructor)
    ▼ tyc-analyse      purity (6 conditions), async checks, comptime sandbox,
    │                  auto-gather data-flow, auto-parallel comprehension rewriter
    ▼ tyc-desugar      Typhon AST → Python AST (merge impl/extend, insert self,
    │                  expand ?, with-chains, gather:, go, pipes, lazy let, newtype,
    │                  freeze, pub, pub * aggregation)
    ▼ tyc-emit         Python codegen + .py.map v2 (per-statement out_line → ty_line)
    ▼ tyc-format       in-process whitespace pass + ruff format wrap
    ▼
    .py + .sourcemaps/*.py.map (+ generated typhon_runtime/ if used)
```

- **`tyc-diagnostics`** uses miette/thiserror for the human-friendly format you see; every code carries a `url(https://typhon.dev/lang/diagnostics/<code>)` deep-link.
- **`tyc-lsp`** is a `tower-lsp-server` backend reusing the same Salsa DB; serves diagnostics, hover, go-to-definition (cross-file via `resolved_module`), completion (including venv-driven member-access introspection cached per session), semantic tokens, "Remove unused import" code action.
- **`tyc-vm`** is the in-process tree-walking interpreter that powers `tyc run`. See [RUNTIME.md](RUNTIME.md).
- **`tyc/`** is the CLI binary that wires it all together with clap v4; subcommands live under `tyc/src/commands/`.
- **`tyc/vendor/`** holds the Ruff fork — `ruff_text_size`, `ruff_source_file`, `ruff_python_trivia`, `ruff_python_ast` (with the `Mutability` extension on assignment nodes), `ruff_python_parser`. See `tyc/vendor/README.md`.

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
| VM produces wrong answer | `tyc-vm` |
| CLI flag, exit code, watch loop | `tyc/src/commands/*.rs` |

The pipeline is **strict** — every crate only depends on its upstream neighbours plus `tyc-diagnostics` and `tyc-db`. There are no skip-level dependencies.

---

## 17. The `typhon_runtime` package

`typhon_runtime/` is a **generated package** the build owns. It's written into the project's output dir on every `tyc build` whenever the desugar pass sets `needs_typhon_runtime = true` (i.e. the emitted code references `Ok`/`Err`/`Result`, `typhon_runtime.tasks.spawn`, `typhon_runtime.lazy.lazy_let`, `typhon_runtime.parallel.map_pure`, or `typhon_runtime.freeze.deep_freeze`).

| File | Surface |
|---|---|
| `__init__.py` | Re-exports `Ok`, `Err`, `Result` |
| `result.py` | `Ok`, `Err`, `Result` dataclasses + `.map`/`.map_err`/`.and_then`/`.or_else` methods (v0.6.0) |
| `tasks.py` | `spawn(coro)` with strong-ref `_BACKGROUND` set + done-callback `discard` |
| `lazy.py` | `_LazyModule` / `lazy_import(name)`; `_LazyValue` / `lazy_let(factory)` |
| `parallel.py` | `map_pure(fn, iterable)` — `concurrent.futures.ThreadPoolExecutor`-backed parallel map; degrades to sequential on GIL-locked CPython |
| `freeze.py` | `deep_freeze(obj)` — recursively replaces `list → tuple`, `dict → MappingProxyType`, `set → frozenset`; raises `TypeError` on un-freezable values at startup |
| `stdlib.py` | Internal helpers used by lowering passes |

**The runtime is generated; users do not edit it and do not `pip install` it.** Regenerated on every `tyc build`. There is no PyPI package — every project ships its own copy alongside the emitted `.py`.

The VM exposes the same surface natively, so `from typhon_runtime import Ok, Err` and `typhon_runtime.tasks.spawn` work in `tyc run`.

See [RUNTIME.md](RUNTIME.md) for the VM's full feature surface and the runtime helper internals.

---

## 18. `.py.map` v2 and source-mapped debugging

Sidecar source map written alongside every emitted `.py` under `<out>/.sourcemaps/` (v0.6.1; legacy adjacent layout still readable). The hand-written `tyc-emit` printer records a `(out_line → ty_line)` mapping at line granularity.

Consumers:

- **`tyc trace`** — rewrites Python tracebacks to point at `.ty` source. Paths with spaces use a longest-candidate walk-left lookup (v0.5.0).
- **`tyc debug --break <ty-file>:<line>`** — translates the Typhon source location through `.py.map` and injects `-c "break build/main.py:N"` into the debugger session.
- **`tyc debug` source-mapping wrapper** — loads every `.py.map` at startup and overrides pdb's `do_list`, `do_where`, `format_stack_entry`, and `prompt` so the **entire** debugger UI reads `.ty` (v0.5.0).
- **`tyc ty`** — same loader as `tyc trace`; rewrites `ty`'s `*.py:LINE[:COL]` diagnostics to point at `.ty` (v0.5.0). `--raw` opts out.
- **`tyc lsp`** — cross-file go-to-definition across the `.ty` / `.py` boundary.

---

## 19. Diagnostics catalog (top tier)

The recurring diagnostic codes and what they actually mean. **See [DIAGNOSTICS.md](DIAGNOSTICS.md) for the exhaustive 67-code reference** — what follows is the daily-driver subset.

| Code | Meaning | Fix |
|---|---|---|
| `tyc::missing_annotation` | Function has no `-> T` or param lacks annotation | Add an explicit type (`-> None` if it returns nothing) |
| `tyc::missing_binding_kind` | Local `=` without `let`/`mut` | Add `let` (default) or `mut` (if rebound) |
| `tyc::immutable_assign` | Reassigning a `let` binding | Change to `mut`, or extract a new `let` |
| `tyc::missing_initialiser` | `let NAME: T` written that is never assigned | Initialise inline, or assign on every non-diverging path |
| `tyc::use_of_uninitialised` | (v0.7.0) Read of `let NAME: T` on a path that didn't assign | Initialise inline, or assign in every non-diverging arm |
| `tyc::no_block_shadow` | Inner `let`/`mut` would shadow an outer binding | Rename inner binding (no block scope in Python) |
| `tyc::pattern_shadows_outer` | `case Wrap(value):` against outer `let value` | Rename the capture (`case Wrap(inner):`) |
| `tyc::nullable_use` | Passing `T?` where `T` required | Narrow with `is None` / `guard` / early-return |
| `tyc::missing_await` | Sync context calling `async def` | Add `await` and make caller `async` |
| `tyc::async_without_await` (warn) | `async def` with no `await` inside | Drop `async` or await something |
| `tyc::blocking_in_async` (warn) | Direct call to known-blocking stdlib inside `async def` | `asyncio.to_thread(...)` or async-native client. Suppressed inside `unsafe:` |
| `tyc::manual_init` | `class` defines `__init__` | Remove it — constructor is generated |
| `tyc::method_in_class_body` (warn) | A `def` inside `class Name:` instead of `impl Name:` | Move into an `impl` block |
| `tyc::field_default_ordering` | (v0.7.0) Non-default field after defaulted one in `class` | Reorder, or use a factory |
| `tyc::frozen_assign` | Writing a field on a `frozen` class | Build a new instance |
| `tyc::missing_field_init` | `X.__new__(X)` escape without every required field assigned | Use the regular constructor |
| `tyc::non_exhaustive_match` | `match` on a sealed union misses a variant | Add the missing `case` or use `case _:` |
| `tyc::invalid_question_op` | `?` inside a non-`Result` function | Change the signature or `match` explicitly |
| `tyc::result_error_mismatch` | `?` returns `Err[E1]` into `Result[T, E2]` | Convert at the boundary with `match` or `.map_err` |
| `tyc::impure_pure_fn` | `@pure` function fails one of the 6 conditions | Refactor or drop `@pure` |
| `tyc::interface_isinstance` | `isinstance(x, SomeInterface)` | Use static narrowing or refactor to a sealed union |
| `tyc::interface_not_conforming` | Type missing/incompatible interface members | Add the method or fix the signature |
| `tyc::stub_mismatch` | `.dty` vs runtime drift detected by `tyc check --stubs` | Update the stub or implementation |
| `tyc::unused_import` (error) | Severity controlled by `[strictness] unused-import` | Remove the import (LSP "Remove unused import" code-action exists) |
| `tyc::orphan_py_import` (warn) | `.py` outside `src/` referenced from a relative import | Move under `src/` or use an absolute import |
| `tyc::stdlib_module_shadow` (warn) | (v0.6.0) `.ty` filename matches a Python 3.13 stdlib top-level module | Rename (e.g. `lang_types.ty`, `records.ty`) |
| `tyc::auto_gather_missed` (advice) | Adjacent awaits look gather-able but a callee lacks `@gatherable` | Decorate the named callee |
| `tyc::newtype_violation` | Bare base-type value flowing into a `newtype` slot or wrong-typed constructor arg | Wrap with the constructor: `UserId(raw_int)` |
| `tyc::resource_not_managed` (warn) | Bare assignment of `open` / `socket.socket` / `sqlite3.connect` / `tempfile.*` without `with` | Wrap in `with` or move into an explicit `try/finally`. Severity controlled by `[strictness] require-with` |
| `tyc::div_by_zero_literal` | Literal-divisor `/ 0`, `// 0`, `% 0` (including `-0.0` and unary-negated zero) | Fix the divisor or guard the call site |
| `tyc::unsafe_value_leak` | A `return x` outside the `unsafe:` block where `x` was declared | Re-assert inside (`let x: T = …`) or re-bind at the boundary (`let typed: T = x`) |
| `tyc::extend_builtin` | `extend list[int]:` (parametric target) | Drop the `[…]`; `extend list:` is the supported form |
| `tyc::duplicate_method` | Two `impl`/`extend` blocks define the same method | Rename, delete, or merge |
| `tyc::pub_name_collision` | (v0.7.0) Two siblings both `pub`-export the same name under `pub *` | Rename one, drop `pub` on one, or use explicit re-exports |
| `tyc::pub_star_outside_init` (advice) | (v0.7.0) `pub *` outside `__init__.ty` is a no-op | Move to `__init__.ty` or remove |
| `tyc::typevar_import_rejected` | `from typing import TypeVar` | Use PEP 695 (`def f[T](...)`) |
| `tyc::typing_alias_deprecated` | `from typing import List/Dict/...` | Use lowercase built-ins |
| `tyc::contains_secret_literal` (warn) | `comptime let *KEY/TOKEN/PASSWORD/SECRET = env(...)` would inline a secret | Read at runtime via `os.environ[...]` |
| `tyc::comptime_env_missing` (or via `tyc::comptime`) | Required env var unset at build time | Set the env var or remove from `[env] required` |
| `tyc::cyclic_type_alias` | `type A = B; type B = A` | Anchor one alias to a concrete type |
| `tyc::class_attr_shadows_slot` (warn) | `class` body with only annotated defaults reads like a constants namespace but emits slot descriptors | Use `ClassVar[T]`, or `pass` body for nullary variants |
| `tyc::tuple_index_out_of_range` | Constant index out of range for fixed-arity tuple | Use an in-range index or change to homogeneous tuple |

`tyc explain <code>` and `tyc explain --list` work offline.

---

## 20. Common pitfalls (the ones every newcomer hits)

See [PITFALLS.md](PITFALLS.md) for the extended ranked list. The top tier:

1. **Forgetting `-> None`.** Sync functions returning nothing still need the annotation.
2. **Writing `x = 1` at function scope.** Locals require `let` or `mut`. (Module top-level is fine — defaults to `let`.)
3. **Calling `find_user(1)` and passing the result somewhere expecting `str`.** It's `str?`. Narrow first.
4. **Putting `def display(self) -> str` inside `class`.** Move to `impl ClassName:` and use `self.NAME` for fields.
5. **Writing `__init__`.** Don't. Use field defaults or a free function.
6. **`from typing import TypeVar`.** Use PEP 695: `def f[T](xs: list[T]) -> T?:`.
7. **`isinstance(x, MyInterface)`.** Rejected — use static narrowing or a sealed union.
8. **`asyncio.create_task(...)` for fire-and-forget.** Use `go f(x)`; the runtime registry holds a strong ref.
9. **`lazy from numpy import array`.** Rejected. Use `lazy import np = numpy` + `np.array(...)`.
10. **`comptime let NOW: float = time.time()`.** Sandbox forbids `time.*`. Compute at runtime with `lazy let`.
11. **`dict.get(k)` typed as `V`.** It's `V?`. Either narrow or use `d[k]`.
12. **Empty list with no annotation.** `let xs: list = []` is a missing-annotation error. Write `list[int]` or similar.
13. **Blocking I/O inside `async def`.** `tyc::blocking_in_async` catches `time.sleep`, `requests.*`, `subprocess.run`, etc.
14. **Expecting `bool` to NOT be `int`.** Since v0.4.0, `bool ⊆ int` — `let x: int = True` works. Reverse is still rejected.
15. **`f = open("x")` without `with`.** `tyc::resource_not_managed` flags this.
16. **`x / 0` literal divisor.** `tyc::div_by_zero_literal`.
17. **`fetch_user(42)` against `def fetch_user(id: UserId)` where `newtype UserId = int`.** Wrap explicitly: `UserId(42)`.
18. **`case value:` shadowing an outer `let value`.** `tyc::pattern_shadows_outer` — rename the capture.
19. **Filename matching a stdlib module (`types.ty`, `json.ty`, `io.ty`).** `tyc::stdlib_module_shadow` — rename.
20. **`pub *` in a non-`__init__.ty` module.** No-op + advice. Move to `__init__.ty`.
21. **Two siblings exporting the same `pub` name.** `tyc::pub_name_collision` under `pub *` aggregation.
22. **`let loaded: Cfg` then reading `loaded` on a path that didn't assign.** `tyc::use_of_uninitialised` (v0.7.0). Initialise inline or assign in every non-diverging arm.
23. **`class P: x: int = 0; y: int` (defaulted before non-defaulted field).** `tyc::field_default_ordering` (v0.7.0). Reorder.
24. **Nullary sealed-union variants written as `case Foo(_):`.** `_` is a positional capture for a class with no positional fields — never matches. Write `case Foo():` (two empty parens) and declare as `class Foo frozen: pass`.
25. **`model X frozen:`.** Doesn't parse — `frozen` is on `class` only.
26. **`let`-shadowing.** Rejected. Use `mut` or a fresh name; never re-bind with `let`.
27. **`lazy let X: T:` colon-block form.** Doesn't parse. Use `lazy let X: T = expr`.
28. **`lazy[list[T]]` return type.** Designed but not yet implemented — use `Iterator[T]`.
29. **Multi-line `|>` pipes without wrapping parens.** Wrap the whole chain in parens.

---

## 21. Recipes — minimum-viable patterns for common tasks

See [COOKBOOK.md](COOKBOOK.md) for canonical patterns extracted from the 68 example exercises and 15 production-shaped apps. Quick pointers:

| Task | Example |
|---|---|
| Hello world / argv | `examples/01-hello-world/` |
| `let` vs `mut` / `T?` narrowing | `examples/02-variables-and-types/` |
| Control flow / comprehensions | `examples/03-control-flow/` |
| Collections / dict.get / tuple destructure | `examples/04-collections/` |
| PEP 695 generics / `Callable` / closures | `examples/05-functions-and-generics/` |
| `class` / `class frozen` / `model` / `impl` / `extend` | `examples/06-classes-and-models/` |
| `Result` / `?` / `with`-chain | `examples/07-error-handling/` |
| Sealed unions + exhaustive match | `examples/08-sealed-unions-match/` |
| Structural interfaces | `examples/09-interfaces/` |
| Pipes + guards | `examples/10-pipes-and-guards/` |
| `comptime let` config from env | `examples/15-comptime-config/` |
| `model` + JSON load via `model_validate` | `examples/17-file-io-json/` |
| Subclassing stdlib (`logging.Formatter`) | `examples/20-logging/` |
| Argparse + sealed-union command + match dispatch | `examples/21-cli-tool/` |
| Async + Result | `examples/23-async-basics/` |
| `gather:` + `@gatherable` + `go` | `examples/24-async-gather-and-go/` |
| FastAPI server with `model` + DI | `examples/28-fastapi-server/` |
| `lazy import np = numpy` | `examples/29-numpy-arrays/` |
| `lazy import torch = torch` + `torch.Tensor` annotations | `examples/33-pytorch-tensors/` |
| Anthropic client (Result-wrapped) | `examples/38-llm-anthropic/` |
| Tool-use loop with `unsafe:` AST walker | `examples/40-llm-tool-use/` |
| Generic agent framework with `Callable` fields | `examples/43-agent-framework/` |
| Multi-file mini app (FastAPI + SQLite + Anthropic) | `examples/47-mini-app/` |
| `newtype` IDs across boundaries | `examples/48-newtype-ids/` |
| Generic sealed-union linked list | `examples/50-linked-list/` |
| `@contextmanager` factories | `examples/58-context-managers/` |
| JSON-RPC with newtype IDs + `unsafe:` boundary coercion | `examples/68-json-rpc-builder/` |
| Pytest + match on Result | `examples/testing/` |
| Production-shaped apps (15) | `examples/apps/01..15-*/` |

---

## 22. CI integration

`tyc check` is the CI-recommended primary gate — runs everything up to the analyser without emitting `.py`. Failure cases:

- Any `tyc::*` diagnostic at `"error"` severity.
- Required env var missing (`comptime let` fails when `DATABASE_URL` etc. is unset, if listed in `[env] required`).
- `tyc check --stubs` drift if you ship `.dty` stubs.

Recommended pipeline:

```yaml
- run: tyc check src/                  # primary gate
- run: tyc check --stubs               # if you ship .dty stubs
- run: tyc ty                          # second-opinion (needs `pip install ty`)
- run: tyc stubtest                    # runtime probe (needs mypy in the venv)
```

`tyc ty` runs Astral's `ty` checker against the emitted Python with `.ty` path attribution. `tyc stubtest` runs mypy's stubtest against the emitted `.pyi`.

---

## 23. Authoring Typhon code as Claude

When you edit `.ty` files in this repo or a downstream project:

1. **Read the relevant guide first** (`docs/guides/`) — every feature has a worked example and its emitted Python listed there. Cross-reference before guessing syntax.
2. **Annotate everything.** If `tyc check` would flag it, write the annotation. There is no implicit `Any` fallback.
3. **Reach for `let` before `mut`.** Only switch to `mut` when you actually need to rebind.
4. **Prefer `Result[T, E]` over `try/except`** anywhere errors are expected (parsing, lookups, validation). Use `try` only at the boundary into untyped libraries.
5. **Prefer sealed unions over inheritance** for closed sets of variants. They give you exhaustive `match`; subclassing doesn't.
6. **Methods go in `impl` blocks**, never inside the `class`. Explicit `self`; access fields via `self.NAME`.
7. **`extend` for cross-module method addition**, `extend BUILTIN:` for static-only built-in extensions.
8. **`gather:` only for genuinely independent awaits.** If one depends on another's value, leave them sequential.
9. **`go` for fire-and-forget**, never `asyncio.create_task` directly.
10. **`lazy import name = module`** for expensive optional deps; never `lazy from ... import ...`.
11. **`comptime let` for build-time constants** (especially required env vars). Don't put secrets there — use `os.environ[...]` at runtime.
12. **`@pure` only when the six conditions hold.** Mark `@memo` separately or use `@pure(memo=True)`. Never silently rely on `auto-memoise` for code others read.
13. **`unsafe:` is a *lexical* region.** Re-assert types at the boundary, don't smuggle `Unsafe[T]` outward. Always end the block with a re-assertion or an unreachable raise.
14. **For multi-file projects, lean on `pub` + `pub *`.** See [PACKAGING.md](PACKAGING.md).
15. **After significant edits, run `tyc fmt src/ && tyc check src/`** and read the diagnostics. The checker is the source of truth.
16. **Read the emitted Python** for any non-trivial feature you haven't seen lower before (`tyc build` then look at `build/*.py`). The lowering is the spec.

When you edit the Rust compiler:

1. **Each diagnostic is registered once** in `tyc-diagnostics`. Search for the code (`rg "TYC_CODE_NAME" tyc/crates`) to find every site that emits it.
2. **Salsa queries** in `tyc-db` are the cache boundary; if a value should be incrementally tracked, it goes through a query.
3. **The vendored Ruff fork** under `tyc/vendor/` is on a clean branch and tracks upstream loosely; do not edit it without a clear note in `tyc/vendor/README.md`.
4. **Run `cargo test --workspace`** before pushing; the LSP tests use `tower-lsp-server`'s harness and the parser tests round-trip a corpus.

---

## 24. Quick reference — Typhon-specific syntax

| Feature | Syntax |
|---|---|
| Immutable local | `let x: int = 5` |
| Mutable local | `mut x: int = 0` |
| Declare-only let (v0.7.0) | `let loaded: Cfg` (assigned later on every non-diverging path) |
| Deep-immutable module binding | `freeze let CFG = {...}` |
| Public modifier | `pub def f(...) -> T: ...` |
| Package wildcard re-export | `pub *` in `__init__.ty` |
| Newtype | `newtype UserId = int` |
| Nullable type | `T?` (sugar for `Optional[T]`) |
| Generic fn (PEP 695) | `def f[T, U](x: T) -> U:` |
| Generic class (PEP 695) | `class Box[T]:` |
| HKT scaffold (v0.5.0) | `class Functor[F[_]]:` |
| Frozen class | `class Point frozen:` |
| Plain class (no decorator) | `plain class Bag:` |
| Raw class (framework base) | `class! MyModel(nn.Module):` |
| Methods | `impl Foo:` block separate from `class Foo:` |
| Cross-module method add | `extend Foo:` |
| Built-in extension | `extend str: def slug(self) -> str: ...` |
| Boundary type | `model X:` (Pydantic-backed) |
| Interface (structural) | `interface Drawable: def draw(self) -> None` |
| Sealed union | `type Shape = Circle \| Rectangle` |
| Distributed impl | `impl Shape: def area(self) -> float: ...` |
| Pattern match | `match s: case Circle(radius): ...` |
| Result success | `Ok(value)` |
| Result failure | `Err(error)` |
| Result propagate | `let x: int = parse()?` |
| Multi-Result chain | `with a = r1?, b = r2?: ... else err: ...` |
| Result combinators | `r.map(f) / r.map_err(g) / r.and_then(h) / r.or_else(k)` |
| Guard / early return | `guard u = maybe else: return default` |
| Pipe | `value \|> f() \|> g()` |
| Compile-time const | `comptime let PORT: int = int(env("PORT", "8080"))` |
| Compile-time fn | `comptime def feature(name: str) -> bool: ...` |
| Compile-time type | `comptime let T: type = int` |
| Deferred import | `lazy import np = numpy` |
| Deferred module-level | `lazy let CFG: Config = load()` |
| Concurrent await | `gather: a = f1(); b = f2()` |
| Best-effort gather | `gather(strategy="best-effort"): ...` |
| Fire-and-forget | `go background_task(x)` |
| Capture handle | `go background_task(x) -> task` |
| Gather marker | `@gatherable async def ...` |
| Pure assertion | `@pure def f(...) -> T:` |
| Memoised | `@memo def fib(n: int) -> int:` |
| Type-check escape | `unsafe: ...` (followed by re-assertion or unreachable raise) |
| Entry guard | `if __name__ == "__main__": main()` |

---

## 25. Further reading

Inside this repo:

- **`docs/long-term-plan.md`** — the canonical design doc.
- **`docs/architecture.md`** — pipeline + crate-by-crate breakdown.
- **`docs/vm.md`** — VM feature surface and design rationale.
- **`docs/prior-art.md`** — TypeScript, rust-analyzer, ty, Pyrefly, oxc, Ruff influence.
- **`docs/risks.md`** — what we expect to bite us.
- **`docs/roadmap.md`** — phased delivery (Phase 6 — Python-annoyances surface complete).
- **`docs/findings.md`** — consolidated stress-test findings.
- **`docs/ty-integration.md`** — how `tyc ty` cooperates with Astral's checker.
- **`docs/performance-baseline.md`** — measured numbers.
- **`TYPE_SYSTEM_FRONTIER.md`** — open frontier work (full HKT unification, general inter-procedural field-init audit).
- **`CHANGELOG.md`** — every release.
- **`stress/`** — stress-test corpora (multi-round campaigns).
- **`tyc/vendor/README.md`** — Ruff fork rationale.
- **`editors/vscode/README.md`** — reference VS Code extension.

Sibling files in this skill:

- **[REFERENCE.md](REFERENCE.md)** — every syntactic form with emitted Python.
- **[CLI.md](CLI.md)** — verbose subcommand reference.
- **[PITFALLS.md](PITFALLS.md)** — extended pitfalls catalogue.
- **[DIAGNOSTICS.md](DIAGNOSTICS.md)** — exhaustive `tyc::` code catalog.
- **[COOKBOOK.md](COOKBOOK.md)** — canonical patterns from `examples/`.
- **[RUNTIME.md](RUNTIME.md)** — the generated `typhon_runtime` package and the in-process VM.
- **[PACKAGING.md](PACKAGING.md)** — multi-file projects, `__init__.ty`, `pub *` aggregation.
