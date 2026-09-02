# Typhon — Syntactic Forms Reference

Every Typhon-specific form, listed side-by-side with the Python it lowers to. For background and design rationale, see `docs/language.md` and `docs/long-term-plan.md`.

**Convention:** Typhon source on the left or above; emitted Python on the right or below. Where formatting matters, code is shown verbatim from the printer.

**Current release: v1.0.0-alpha.9.** Forms tagged with a version annotation (`(v0.5.0)` etc.) landed in that release; everything else has been in Typhon since v0.1.0 or v0.2.0. **v1.0.0-alpha** is the first feature-complete alpha — the type-system frontier (HKT unification, user-generic variance inference, the inter-procedural field-init audit) plus the `rescue` exception-boundary sugar (the only new everyday syntax form). **v1.0.0-alpha.2** is a type-checker soundness + VM-parity sweep that adds three conservative diagnostics (`tyc::not_a_context_manager`, `tyc::raise_non_exception`, `tyc::frozen_inheritance_conflict`) and types more positions (slice reads, subscript assignments, tuple-unpack and `match` captures, walrus bindings, parameter defaults) — no new syntax. v0.15.7 deepens compile-time third-party checking (method-call arity-checking + introspection-robustness fixes) and clears more type-checker false positives, with compiler-perf passes — no new syntax. The new language *forms* since v0.9.0 are the `enum` keyword (v0.11.0) and the `as!` checked boundary cast (§13.1, v0.14.0); v0.10.0–v0.13.x are otherwise VM-completeness, compile-time-checking, and CPython-parity work (v0.13.1 / v0.13.2 are bug patches), and v0.14.0 also adds the opt-in `[emit] traceback-remap` config — not new syntax. v0.14.1–v0.15.7 are additive too: cross-module shape propagation completeness (v0.14.1), the on-by-default `tyc::gather_opportunity` advice + cross-module `auto-gather` (v0.14.2), live LSP config refresh (v0.14.3), the `as!` cast composing in **any** expression position plus the `try_result(thunk[, on_err])` prelude combinator and compiler-bundled `.dty` stubs (v0.15.0), perf/bugfix patches (v0.15.1 / v0.15.2), cross-module interface conformance + `pub comptime let` (v0.15.4), cross-module `extend BUILTIN:` propagation (v0.15.5), a stress-test robustness sweep clearing type-checker false positives (custom-exception lowering, `match` exhaustiveness, `isinstance`-narrowed containers, iterator-protocol conformance, `dict`-view set ops) and VM parity gaps, plus flow-sensitive attribute narrowing and a VM `abc` shim (v0.15.6); and third-party *method*-call arity-checking plus a batch of introspection-robustness and type-checker false-positive fixes — proxy-member introspection survival, implicit-Optional `x: T = None`, multi-segment `pkg.sub.Thing()` calls, `Counter +/- Counter`, `plain class`/`class!` custom-`__init__` kwargs, `__call__`→`Callable`, and a VM `str.isupper()`/`islower()` parity fix — with compiler-perf passes (v0.15.7). **v1.0.0-alpha.3** (release-readiness remediation: licensing/packaging, complete duplicate-location error reporting, four flow-narrowing invalidation fixes, six VM ↔ CPython parity fixes, tooling robustness), **v1.0.0-alpha.4** (the H5 scope-blind class-unification soundness guard, longest-first secret-name matching, supply-chain hygiene), and **v1.0.0-alpha.5** (VM performance Tier 1, the `[optimise]` profile + `tyc build -O`, the `tyc::perf_*`/`lazy_import_opportunity` advice-lint family, a free-threading parallelisation wave, native PEP 810 lazy imports on 3.15 targets) likewise add **no new syntax** — every alpha.5 rewrite is opt-in or advice-only.

---

## 1. Bindings

### 1.1 Local bindings

```python
# Typhon
def demo() -> None:
    let pi: float = 3.14159
    mut count: int = 0
    count = count + 1
```

```python
# Emitted Python
def demo() -> None:
    pi: float = 3.14159
    count: int = 0
    count = count + 1
```

The `let`/`mut` keyword is enforced at compile time and erased at emit. Reassignment of a `let` is `tyc::immutable_assign`.

### 1.2 Module-level bindings

```python
# Typhon
PI: float = 3.14159           # implicitly let
mut FEATURE_FLAG: bool = False
```

```python
# Emitted Python
PI: float = 3.14159
FEATURE_FLAG: bool = False
```

Inside a function, the kind is always explicit. At module top level, it defaults to `let` unless declared `mut`.

### 1.3 Typed tuple unpacking (v0.3.1)

```python
# Typhon
let (a: int, b: str) = func(x, y)
let (a: int, b)      = pair()                   # mixed
let (xs: list[int], ys: list[int]) = split()    # compound annotations
```

```python
# Emitted Python (sketch)
__typhon_unpack_0__ = func(x, y)
a: int = __typhon_unpack_0__[0]
b: str = __typhon_unpack_0__[1]
```

Top-level-comma split inside the annotation pair survives compound annotations (`list[int]`, `dict[str, int]`, `tuple[float, ...]`). Mixed forms — annotated leg gets the type, un-annotated leg flows through inference.

### 1.4 Declare-only `let NAME: T` (v0.7.0)

```python
# Typhon
def parse(raw: str) -> Result[Cfg, str]:
    let loaded: Cfg                          # declare-only
    match _load(raw):
        case Ok(v):  loaded = v              # first assignment IS the initialiser
        case Err(e): return Err(e)           # diverging arm — excluded from intersection
    return Ok(loaded)
```

```python
# Emitted Python
def parse(raw: str) -> Result[Cfg, str]:
    loaded: Cfg                              # declaration carries through
    match _load(raw):
        case Ok(v):  loaded = v
        case Err(e): return Err(e)
    return Ok(loaded)
```

The resolver tracks each uninitialised `let` declaration's span; first assignment silently succeeds (it IS the initialiser). A second assignment fires `tyc::immutable_assign`. Sibling `match` arms and sibling `if`/`elif`/`else` bodies each count as a separate first-assignment path; the union is taken across non-diverging arms. Reads on a path that hasn't assigned fire `tyc::use_of_uninitialised`. `mut NAME: T` without initialiser is also accepted (any number of subsequent assignments legal).

### 1.5 `freeze let` — deep-immutable bindings (v0.3.0)

```python
# Typhon (module level)
freeze let CONFIG = {"port": 8080, "hosts": ["a", "b"]}
```

```python
# Emitted Python
from typhon_runtime.freeze import deep_freeze as __typhon_freeze__

CONFIG = __typhon_freeze__({"port": 8080, "hosts": ["a", "b"]})
```

`deep_freeze(value)` recursively converts `list → tuple`, `dict → MappingProxyType`, `set → frozenset`; descends into existing immutable containers for nested values; passes through primitives and frozen dataclasses; raises `TypeError` on file handles, sockets, generators, and non-frozen dataclasses. The binding name itself is `let`-locked as well.

Stacks with `pub` (v0.6.0): `pub freeze let X = …` parses.

### 1.6 `pub` — module visibility marker (v0.3.0)

```python
# Typhon
pub let API_VERSION: str = "v1"
pub class Client:
    host: str
pub def connect(host: str) -> Client: ...

let _internal_default_port: int = 8080
```

```python
# Emitted Python
__all__ = ["API_VERSION", "Client", "connect"]

from dataclasses import dataclass

API_VERSION: str = "v1"

@dataclass(slots=True)
class Client:
    host: str

def connect(host: str) -> Client: ...

_internal_default_port: int = 8080
```

The synthesised `__all__` is emitted once at the top of the file (after imports). `pub` stacks with every modifier keyword: `pub class X frozen:` (postfix modifier), `pub model`, `pub let`, `pub mut`, `pub freeze let`, `pub newtype`, `pub interface`, `pub type`, `pub def`, `pub async def`.

### 1.7 `pub *` — package-level re-export aggregation (v0.7.0)

```python
# src/mypkg/__init__.ty
pub *
```

```python
# build/mypkg/__init__.py (sketch)
from .ids import UserId, PostId
from .user import User, make_user
from .post import Post

__all__ = ["UserId", "PostId", "User", "make_user", "Post"]
```

Aggregates every direct-sibling module's `pub` declarations alphabetically by basename, plus every direct sub-package's effective public surface (transitive, cycle-safe). See [PACKAGING.md](PACKAGING.md) for the full surface.

### 1.8 `newtype` — nominal aliases (v0.3.0)

```python
# Typhon
newtype UserId = int
newtype PostId = int

def fetch_user(id: UserId) -> User: ...

let uid: UserId = UserId(42)
fetch_user(uid)
let raw: int = uid             # ✅ UserId → int flows
# fetch_user(42)               # ❌ tyc::newtype_violation
```

```python
# Emitted Python
from typing import NewType

UserId = NewType("UserId", int)
PostId = NewType("PostId", int)

def fetch_user(id: UserId) -> User: ...

uid: UserId = UserId(42)
fetch_user(uid)
raw: int = uid
```

`NewType` is a zero-cost wrapper at runtime (the call returns its argument unchanged); the asymmetric assignability is a Typhon-checker rule. The constructor type-checks its argument against the base — `UserId("forty-two")` is `tyc::newtype_violation`.

**Same-newtype arithmetic preserves the newtype** (v0.7.0): `LogIndex(a) + LogIndex(b)` is `LogIndex` across `+ - * // % **`. `LogIndex(a) + 1` (literal of base) is also `LogIndex`. `/` always widens to `float`. Two distinct newtypes sharing a base (`LogIndex + Term`) fire `tyc::operator_type_mismatch`.

---

## 2. Optional types

```python
# Typhon
def find(id: int) -> str?:
    if id == 1:
        return "Alice"
    return None

let raw: str? = find(2)
if raw is not None:
    print(raw)                # narrowed to str

guard r = find(2) else: return
print(r)                      # narrowed to str
```

```python
# Emitted Python
def find(id: int) -> str | None:
    if id == 1:
        return "Alice"
    return None

raw: str | None = find(2)
if raw is not None:
    print(raw)

if find(2) is None:
    return
r: str = find(2)              # roughly; the emitter binds via a temp
print(r)
```

Narrowing forms the checker recognises: `is None`, `is not None`, `isinstance(x, T)`, `guard`, early-return `if x is None: return`, exhaustive `match`, **ternary** `body if test else orelse` (v0.7.0), De Morgan refinement (`if not (A or B): return` narrows both operands afterwards, v0.4.0), `while` test-implied narrowings applied to body (v0.3.0), **`assert x is not None`** (v0.9.0; the standard Python static-checker idiom), **post-while-loop narrowing** (v0.9.0; after `while y is None: y = load()` with no `break`, `y` is non-`None` afterwards), and **`while True:` reachability** (v0.9.0; a body that always returns/raises with no `break` marks the post-loop point as unreachable so `missing_return` doesn't fire).

---

## 3. Classes

### 3.1 Plain class → dataclass

```python
# Typhon
class User:
    id: int
    name: str = "anon"
    email: str?
    tags: list[str] = []
```

```python
# Emitted Python
from dataclasses import dataclass, field

@dataclass(slots=True)
class User:
    id: int
    name: str = "anon"
    email: str | None = None
    tags: list[str] = field(default_factory=list)
```

`slots=True` is the default. Instances do not carry a per-object `__dict__`; typos at attribute write sites raise `AttributeError`.

Mutable literal defaults (`[]`, `{}`, `set()`, `list()`, `dict()`) auto-rewrite to `dataclasses.field(default_factory=...)`. Applies to `class` and `class frozen`; skipped for `model`, `interface`, and `class!`.

### 3.2 Frozen class

```python
# Typhon
class Point frozen:
    x: float
    y: float
```

```python
# Emitted Python
@dataclass(slots=True, frozen=True)
class Point:
    x: float
    y: float
```

`frozen=True` only blocks **field reassignment**. Nested mutable containers can still be mutated. Use `tuple` / `frozenset` for stronger guarantees.

**Frozen + inheritance ordering (v0.9.0 cheatsheet clarification):** when combining `frozen` with a base class, the modifier comes **between** the class name and the base list:

```python
class Square frozen(Shape):    # ✓ parses
    side: float

# `class Square(Shape) frozen:` does NOT parse.
```

### 3.3 `model` → Pydantic

```python
# Typhon
model ApiUser:
    id: int
    email: str
    name: str = "anon"
```

```python
# Emitted Python
from pydantic import BaseModel, ConfigDict

class ApiUser(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: int
    email: str
    name: str = "anon"
```

`extra="forbid"` is the default; override globally via `[emit] model-extra = "allow" | "ignore"`. Do not write `__init__` — the constructor is generated. **`model X frozen:` does NOT parse** — `frozen` is on `class` only.

### 3.4 `plain class` — bare class (no decorator)

```python
# Typhon
plain class Bag:
    items: list[str]
```

```python
# Emitted Python
class Bag:
    items: list[str]
```

No `@dataclass`, no synthesised `__init__`. For metaclass-driven libraries (Textual, Django ORM, SQLAlchemy declarative).

### 3.5 `class!` — raw class with synthesised `__init__`

```python
# Typhon
class! MyModel(nn.Module):
    layer1: nn.Linear
    layer2: nn.Linear


impl MyModel:
    def forward(self, x: torch.Tensor) -> torch.Tensor:
        let h: torch.Tensor = torch.relu(self.layer1(x))
        return self.layer2(h)
```

```python
# Emitted Python (sketch)
class MyModel(nn.Module):
    layer1: nn.Linear
    layer2: nn.Linear

    def __init__(self, layer1: nn.Linear, layer2: nn.Linear) -> None:
        super().__init__()
        self.layer1 = layer1
        self.layer2 = layer2

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        h: torch.Tensor = torch.relu(self.layer1(x))
        return self.layer2(h)
```

For framework bases that own their own `__init__`. No `@dataclass` decorator. `__init__` auto-synthesised when the body has no user `__init__` and ≥1 base is present: calls `super().__init__()` first, then assigns fields. Class-level field defaults are stripped from the body when `__init__` is synthesised. A hand-written `__init__` is preserved verbatim.

### 3.6 Methods via `impl`

```python
# Typhon
class User:
    id: int
    name: str
    email: str?

impl User:
    def display(self) -> str:
        return f"{self.name} <{self.email}>" if self.email is not None else self.name

    def is_admin(self) -> bool:
        return self.id == 0
```

```python
# Emitted Python
@dataclass(slots=True)
class User:
    id: int
    name: str
    email: str | None = None

    def display(self) -> str:
        return f"{self.name} <{self.email}>" if self.email is not None else self.name

    def is_admin(self) -> bool:
        return self.id == 0
```

`self` is explicit; access fields via `self.NAME`. The desugarer merges `impl` blocks into the class body.

### 3.7 Methods via `extend`

```python
# domain/user.ty
class User:
    id: int
    name: str

# analytics/user_metrics.ty
extend User:
    def tracking_id(self) -> str:
        return f"user-{self.id:08d}"
```

Emits identically to having both blocks in one file. Desugar collects them across the project.

### 3.8 Distributed `impl` on a sealed-union alias (v0.6.0)

```python
# Typhon
type Event = TaskStarted | TaskFinished | TaskFailed

impl Event:
    def task_id(self) -> int:
        match self:
            case TaskStarted(tid): return tid
            case TaskFinished(tid, _): return tid
            case TaskFailed(tid, _): return tid
```

The method body is replicated onto each variant at desugar — every variant ends up with a `task_id` method. `tyc::duplicate_method` fires if the same method exists on both `impl Event:` and `impl TaskStarted:`.

### 3.9 Extending built-ins

```python
# Typhon
extend str:
    def slug(self) -> str:
        return self.strip().lower().replace(" ", "-")

let title: str = "Hello World"
print(title.slug())
print("untyped".slug())              # AttributeError at runtime — no static `str` annotation
```

```python
# Emitted Python (sketch)
def __typhon_ext_str__slug__(self: str) -> str:
    return self.strip().lower().replace(" ", "-")

title: str = "Hello World"
print(__typhon_ext_str__slug__(title))
print("untyped".slug())              # untouched; falls back to native attribute lookup
```

Only call sites whose receiver has a static `str` annotation get rewritten. No monkey-patching. `extend list[int]:` (parametric target) → `tyc::extend_builtin`; use `extend list:`.

### 3.10 Field default ordering (v0.7.0)

```python
# ❌ tyc::field_default_ordering
class Worker:
    name: str
    retries: int = 3
    queue_size: int           # non-default after default
```

```python
# ✅
class Worker:
    name: str
    queue_size: int
    retries: int = 3
```

Synthesised `__init__` follows declaration order; Python rejects a non-default param after a default one. Caught at check time instead of at runtime with a misleading `TypeError`.

### 3.11 Auto-skip framework bases

```python
# Typhon — no @dataclass synthesis because base ends in `Enum`
class Color(Enum):
    RED = 1
    GREEN = 2
    BLUE = 3
```

```python
# Emitted Python
class Color(Enum):
    RED = 1
    GREEN = 2
    BLUE = 3
```

Last identifier segment in `Enum`, `IntEnum`, `StrEnum`, `Flag`, `IntFlag`, `ABC`, `ABCMeta`, `Protocol`, `TypedDict`, `NamedTuple`, `BaseModel`, `App` — auto-skipped. Extend via `[emit] skip-decoration-bases = ["MyBase", ...]`. Auto-skip drops the decorator only; it does not synthesise `__init__`. Use `class!` when you need both.

### 3.12 `enum` keyword (v0.11.0)

`enum Name:` is the idiomatic declaration form for a fixed set of named members — sugar over `enum.Enum`, the same way `model` is sugar over `pydantic.BaseModel`. Bare members auto-number with `enum.auto()`; explicit `MEMBER = value` is preserved, and a subsequent bare member resumes `enum.auto()` numbering from the last value — standard CPython `enum` semantics (e.g. `A = 10` then a bare `B` yields `B = 11`, not `2`).

```python
# Typhon
enum Shape:
    CIRCLE
    SQUARE
    TRIANGLE

enum Color:
    RED = 1
    GREEN = 2
    BLUE = 4
```

```python
# Emitted Python
import enum


class Shape(enum.Enum):
    CIRCLE = enum.auto()
    SQUARE = enum.auto()
    TRIANGLE = enum.auto()


class Color(enum.Enum):
    RED = 1
    GREEN = 2
    BLUE = 4
```

`tyc-syntax` preprocesses the `enum` header / body, `tyc-emit` injects `import enum` when an `enum.*` base is present, `tyc-resolve` adds `enum` to the builtin prelude (so the form type-checks before the import is rewritten in), and `tyc fmt` round-trips both the header and the members. The VM (v0.11.0) resolves `enum.Enum` / `enum.auto()` natively, materialises members in declaration order, and matches CPython's `<Shape.CIRCLE: 1>` member repr. (Subclassing `enum.Enum` directly via the §3.11 auto-skip path still works; `enum Name:` is the preferred spelling.)

---

## 4. Sealed unions and `match`

### 4.1 Long form

```python
# Typhon
type Shape = Circle | Rectangle | Triangle

class Circle:    radius: float
class Rectangle: width: float; height: float
class Triangle:  base: float;  height: float

def area(s: Shape) -> float:
    match s:
        case Circle(radius):       return 3.14159 * radius * radius
        case Rectangle(w, h):      return w * h
        case Triangle(b, h):       return 0.5 * b * h
```

```python
# Emitted Python
from dataclasses import dataclass

@dataclass(slots=True)
class Circle:
    radius: float
@dataclass(slots=True)
class Rectangle:
    width: float
    height: float
@dataclass(slots=True)
class Triangle:
    base: float
    height: float

Shape = Circle | Rectangle | Triangle

def area(s: Shape) -> float:
    match s:
        case Circle(radius):       return 3.14159 * radius * radius
        case Rectangle(w, h):      return w * h
        case Triangle(b, h):       return 0.5 * b * h
```

Exhaustiveness is a compile-time check. `case _:` opts out for the rest of the union.

### 4.2 The only form — a `type` alias over variant classes

There is **no `sealed union NAME:` keyword block**; a sealed union is just a
`type` alias over separately-declared variant classes:

```python
type Shape = Circle | Square

class Circle:
    radius: float
class Square:
    side: float
```

(Earlier docs showed a `sealed union Shape:` block form — it does not parse.)

### 4.3 Parametric sealed unions

```python
type EventEnvelope[T] = RecordEnv[T] | WatermarkEnv | BarrierEnv

class RecordEnv[T]:
    payload: T
class WatermarkEnv:
    ts: int
class BarrierEnv:
    id: int
```

Some variants refer to `T`, others don't. `T` flows through `match` arms (and through cross-module generic method dispatch in v0.7.0).

### 4.4 Nullary variants

```python
type State = Red | Yellow | Green

class Red:    pass
class Yellow: pass
class Green:  pass

match s:
    case Red():    ...
    case Yellow(): ...
    case Green():  ...
```

`case Foo()` (two empty parens) — **not** `case Foo(_)`, which is a positional capture for a class with no positional fields and never matches.

### 4.5 Keyword patterns (v0.6.0)

```python
match event:
    case TaskFinished(task_id=tid, output=out):
        print(f"#{tid} → {out}")
    case TaskFailed(task_id=tid, reason=r):
        print(f"#{tid} failed: {r}")
```

Binds only the named fields, in any order. Survives field additions. Counts toward exhaustiveness coverage the same way positional patterns do.

---

## 5. `Result[T, E]`, `?`, `with`-chains

### 5.1 Result and `?`

```python
# Typhon
def parse_port(raw: str) -> Result[int, str]:
    if not raw.isdigit():
        return Err(f"not a number: {raw}")
    let n: int = int(raw)
    if n < 1 or n > 65535:
        return Err(f"out of range: {n}")
    return Ok(n)

def parse_addr(host: str, raw: str) -> Result[tuple[str, int], str]:
    let port: int = parse_port(raw)?
    return Ok((host, port))
```

```python
# Emitted Python
from typhon_runtime import Ok, Err, Result

def parse_port(raw: str) -> Result[int, str]:
    if not raw.isdigit():
        return Err(f"not a number: {raw}")
    n: int = int(raw)
    if n < 1 or n > 65535:
        return Err(f"out of range: {n}")
    return Ok(n)

def parse_addr(host: str, raw: str) -> Result[tuple[str, int], str]:
    _tmp_0 = parse_port(raw)
    if isinstance(_tmp_0, Err):
        return _tmp_0
    port: int = _tmp_0.value
    return Ok((host, port))
```

`?` does **not** lower to `try/except`. The inline `isinstance(Err)` check is part of the design — stack traces stay clean.

Inline `?` is supported (v0.3.0): `Ok(add(parse(s)?, parse(t)?))` works. `?` inside a comprehension is rejected (v0.3.1).

### 5.2 `with`-chain

```python
# Typhon
def make(uid: int) -> Result[Report, AppError]:
    with user   = db.find(uid)?,
         perms  = check(user)?,
         report = build(user, perms)?:
        return Ok(report)
    else err:
        log.warn(err)
        return Err(err)
```

```python
# Emitted Python (sketch)
def make(uid: int) -> Result[Report, AppError]:
    _r0 = db.find(uid)
    if isinstance(_r0, Err):
        err = _r0.error
        log.warn(err)
        return Err(err)
    user = _r0.value

    _r1 = check(user)
    if isinstance(_r1, Err):
        err = _r1.error
        log.warn(err)
        return Err(err)
    perms = _r1.value

    _r2 = build(user, perms)
    if isinstance(_r2, Err):
        err = _r2.error
        log.warn(err)
        return Err(err)
    report = _r2.value

    return Ok(report)
```

`else err:` is optional. Without it, the first `Err` short-circuits via the enclosing function.

### 5.3 Combinators (v0.6.0)

```python
# Typhon
let toks: Tokens   = tokenize(src).map_err(_lex_to_pipeline)?
let ast:  Ast      = parse(toks).map_err(_parse_to_pipeline)?
let ty:   TypedAst = check(ast).map_err(_type_to_pipeline)?
```

`Ok` and `Err` carry `.map`, `.map_err`, `.and_then`, `.or_else` methods on the runtime classes. Semantics: `Ok.map(f)` transforms value; `Ok.map_err(g)` identity; `and_then` chains a `Result`-returning op on `Ok`; `or_else` recovers from `Err`. Since v0.9.0 these combinators also work under `tyc run` — the VM binds them as native methods via `NativeFn` wrappers that capture the receiver.

### 5.4 `try_result` and `rescue` — exception boundaries

`try_result(thunk[, on_err])` (v0.15.0) turns a throwing call into a `Result` in one expression. `rescue` (v1.0.0-alpha) is the no-lambda, no-`try`/`except` surface for the same bridge, in two forms:

```python
# Typhon — postfix rescue
let n: int = int(raw) rescue e: BadField(field="port", reason=str(e))

# Typhon — block rescue
def load_config(text: str) -> Result[Config, ConfigError]:
    rescue e: BadJson(reason=str(e)):
        let data: dict[str, str] = json.loads(text) as! dict[str, str]
        return Ok(Config(host=data["host"], port=int(data["port"])))
```

```python
# Emitted Python — postfix lowers to try_result + the `?` ladder
__typhon_q_0__ = try_result(lambda: int(raw), lambda e: BadField(field="port", reason=str(e)))
if isinstance(__typhon_q_0__, __typhon_Err__):
    return __typhon_q_0__
n: int = __typhon_q_0__.value

# Emitted Python — block lowers to try/except returning Err
def load_config(text: str) -> Result[Config, ConfigError]:
    try:
        data: dict[str, str] = __typhon_checked_cast__(json.loads(text), dict[str, str])
        return Ok(Config(host=data["host"], port=int(data["port"])))
    except Exception as e:
        return Err(BadJson(reason=str(e)))
```

Postfix `EXPR rescue NAME: ERR` → `try_result(lambda: EXPR, lambda NAME: ERR)?`; lowered in **statement-tail** position, composes with `as!`, works after `return`/`if`/`while`/`assert`. Block `rescue NAME: ERR:` → `try`/`except Exception as NAME: return Err(ERR)` (fixpoint, so nested blocks expand). The block emits a real `Err(...)`, so its error type is checked against the function's declared error type; the postfix form is checked through `?` (`tyc::result_error_mismatch`). Both run under `tyc check` / `tyc run` / `tyc build`; `tyc fmt` round-trips. Worked example: `examples/60-rescue-boundaries/`.

---

## 6. Async and concurrency

### 6.1 `gather:`

```python
# Typhon
async def load(uid: int) -> Dashboard:
    gather:
        user   = fetch_user(uid)
        posts  = fetch_posts(uid)
        notifs = fetch_notifs(uid)
    return Dashboard(user=user, posts=posts, notifs=notifs)
```

```python
# Emitted Python
import asyncio

async def load(uid: int) -> Dashboard:
    async with asyncio.TaskGroup() as _tg:
        _t_user   = _tg.create_task(fetch_user(uid))
        _t_posts  = _tg.create_task(fetch_posts(uid))
        _t_notifs = _tg.create_task(fetch_notifs(uid))
    user   = _t_user.result()
    posts  = _t_posts.result()
    notifs = _t_notifs.result()
    return Dashboard(user=user, posts=posts, notifs=notifs)
```

### 6.2 Best-effort gather

```python
# Typhon
gather(strategy="best-effort"):
    user = fetch_user(uid)
    posts = fetch_posts(uid)
```

```python
# Emitted Python
user, posts = await asyncio.gather(
    fetch_user(uid),
    fetch_posts(uid),
    return_exceptions=True,
)
# user / posts now have type `User | BaseException` etc.
```

### 6.3 `go`

```python
# Typhon
async def signup(email: str) -> User:
    let user: User = await create(email)
    go send_welcome(user)
    return user

async def signup_with_handle(email: str) -> User:
    let user: User = await create(email)
    go send_welcome(user) -> task
    await task
    return user
```

```python
# Emitted Python (sketch)
from typhon_runtime.tasks import spawn

async def signup(email: str) -> User:
    user: User = await create(email)
    spawn(send_welcome(user))
    return user

async def signup_with_handle(email: str) -> User:
    user: User = await create(email)
    task = spawn(send_welcome(user))
    await task
    return user
```

`spawn` registers the task in a strong-ref registry and clears it via a done-callback. **Never** use `asyncio.create_task` directly — weak refs let fire-and-forget tasks be GC'd mid-flight. Multi-line `go expr(...)` parses (v0.7.0).

### 6.4 Async-callable awaits (v0.7.0)

```python
# Typhon
async def middleware(next: Callable[[Req], Awaitable[Resp]], req: Req) -> Resp:
    let resp: Resp = await next(req)        # ✅ unwraps Awaitable[Resp] to Resp
    return resp
```

`await` on a `Callable[..., Awaitable[T]]` / `Callable[..., Coroutine[Y, S, T]]` call unwraps to `T`. Canonical async-middleware shape now works.

### 6.5 `with cm() as r:` typing (v0.7.0)

```python
# Typhon
@contextmanager
def session() -> Iterator[Session]:
    let s: Session = Session()
    try:
        yield s
    finally:
        s.close()


def use_it() -> None:
    with session() as s:
        s.query("SELECT 1")                  # s typed as Session
```

The `with`-as target reads its type from the `@contextmanager` factory's yield-type. `@asynccontextmanager` + `async with` works equivalently. Concrete-class `__enter__` / `__aenter__` return types also propagate. Stdlib stub-only forms (`with open(p) as f:`) fall through to `Unknown`.

---

## 7. Lazy

### 7.1 `lazy import name = module`

```python
# Typhon
lazy import np = numpy

def main() -> None:
    if len(sys.argv) > 1:
        let arr: np.ndarray = np.array([1, 2, 3])
```

```python
# Emitted Python (sketch)
import importlib.util
import sys as _sys

_spec = importlib.util.find_spec("numpy")
_loader = importlib.util.LazyLoader(_spec.loader)
_spec.loader = _loader
np = importlib.util.module_from_spec(_spec)
_sys.modules["numpy"] = np
_loader.exec_module(np)         # deferred — only runs on first attribute access via the proxy
```

The exact emission uses a small thread-safe proxy class so concurrent first accesses serialise around the underlying load.

`lazy from numpy import array` is **rejected at parse time** (`tyc::lazy_usage`). Use `lazy import numpy` and dotted access.

On a **3.15+ `[python] target`**, the lowering above is skipped entirely: `lazy import np = numpy` instead emits the native PEP 810 `lazy import numpy as np` statement — no proxy class, no `typhon_runtime` involvement. 3.13 / 3.14 targets keep the proxy-class emission shown above, byte-for-byte.

### 7.2 Module-level `lazy let`

```python
# Typhon
lazy let CONFIG: Config = load_config_from_disk()
```

```python
# Emitted Python (sketch)
from typhon_runtime.lazy import lazy_let as __typhon_lazy_let

CONFIG: Config = __typhon_lazy_let(lambda: load_config_from_disk())
```

Sentinel-cached one-shot. Thread-safe via internal lock.

### 7.3 Class-body `lazy let`

```python
# Typhon
class Loader:
    path: str

impl Loader:
    lazy let cfg: Config = parse(self.path)
```

```python
# Emitted Python
@functools.cached_property
def cfg(self) -> Config:
    return parse(self.path)
```

Per-instance scope is the intended semantics.

### 7.4 `lazy[T]` return type — designed but NOT yet supported

```python
# Designed
def primes(n: int) -> lazy[list[int]]:
    ...
    return [i for i in range(2, n + 1) if sieve[i]]
```

The form parses but is **unimplemented today**. Use `Iterator[int]` directly:

```python
def primes(n: int) -> Iterator[int]:
    ...
    yield from (i for i in range(2, n + 1) if sieve[i])
```

---

## 8. `comptime`

```python
# Typhon
comptime let PORT: int = int(env("PORT", "8080"))
comptime let DB_URL: str = env("DATABASE_URL")
comptime let URL: str = "/".join([host, path])       # v0.3.1 str.join allowed
comptime let T: type = int                            # v0.5.0 types-as-values; v0.9.0: lowers to PEP 695 `type T = int`

comptime def feature(name: str) -> bool:
    return env(f"FEATURE_{name.upper()}", "0") == "1"

comptime let DARK_MODE: bool = feature("dark_mode")
```

```python
# Emitted Python (literals inlined at build time; comptime def preserved)
PORT: int = 8080
DB_URL: str = "postgresql://..."
URL: str = "example.com/api"
DARK_MODE: bool = True

def feature(name: str) -> bool:
    return env(f"FEATURE_{name.upper()}", "0") == "1"
```

`comptime def` functions are **also preserved** in the emitted `.py` so they remain callable at runtime.

Since v0.9.0 `comptime let T: type = <Type>` lowers to a PEP 695 `type T = <Type>` alias statement so `T` is substitutable wherever a type is expected. `tyc check` also runs the substitution before parsing the resolved module so check, build, and VM all see the same shape. And: `comptime let X = ...` value bindings now inline in the VM via the substitution pass shared with `tyc build`, so `tyc run` no longer crashes with `NameError: env is not defined` on `comptime let PORT = int(env(...))`.

The sandbox allows: arithmetic, string ops (including `str.join` v0.3.1), `env(name, default?)`, container construction with subscript including negative indexing, ternaries, `if`/`elif`/`else`, types-as-values (8 primitive heads), calls to other `comptime def` functions. Forbids: loops, exceptions, `with`-blocks, classes, free variables, `*args`/`**kwargs`/defaults, I/O, network, subprocess, random, time, uuid, arbitrary imports. Recursion depth capped at 64.

Required env vars are declared in `typhon.toml`:

```toml
[env]
required = ["DATABASE_URL"]
```

Missing required env → build fails with `tyc::comptime` (named `comptime_env_missing` in some docs).

---

## 9. Generics — PEP 695

```python
# Typhon
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]

class Box[T]:
    value: T

impl[T] Box[T]:
    def get(self) -> T:
        return self.value

    def map[U](self, f: Callable[[T], U]) -> Box[U]:
        return Box(value=f(self.value))

type Vec[T] = list[T]
type Pair[A, B] = tuple[A, B]
```

```python
# Emitted Python (PEP 695 preserved on 3.12+ targets)
def first[T](xs: list[T]) -> T | None:
    if len(xs) == 0:
        return None
    return xs[0]

@dataclass(slots=True)
class Box[T]:
    value: T

    def get(self) -> T:
        return self.value

    def map[U](self, f: Callable[[T], U]) -> "Box[U]":
        return Box(value=f(self.value))

type Vec[T] = list[T]
type Pair[A, B] = tuple[A, B]
```

**Cross-module generic method dispatch propagates class TypeVars** (v0.7.0): `s: Stream[int]; s.map(f)` records `Callable[[int], U]` as the expected parameter and returns `Stream[U]`. Field access also propagates: `r: RecordEnv[int]; r.payload` is `int`.

**Read-view covariance** (v0.9.0): `list[Subclass]` / `tuple[Subclass]` / `set[Subclass]` / `frozenset[Subclass]` flow into `Sequence[Super]` / `Iterable[Super]` / `Iterator[Super]` / `Collection[Super]` / `Container[Super]` / `Reversible[Super]` when `Subclass` inherits `Super`. `dict[K, V]` is K-invariant + V-covariant under `Mapping[K, V]`.

**Variant → parametric sealed union assignability** (v0.9.0): given `type LL[T] = Cons[T] | Nil`, an `Cons[T]` value flows into a `LL[T]` slot. Required for recursive ADT walks like `mut cur: LL[T] = self`.

**`func[T](args)` explicit type instantiation** (v0.9.0): rejected at check time with a clear error pointing users at the bidirectional inference pattern (drop the `[T]` — `T` is inferred from the argument). Was previously a runtime `TypeError`.

**Never** import `TypeVar` from `typing`. The PEP 695 path is the only one.

### Annotation policy for `*args` / `**kwargs` (v0.9.0)

Rule 1 (every parameter annotated) is enforced on `*args` / `**kwargs` too. Canonical idiom for genuinely variadic functions (typically generic decorators):

```python
def trace[R](f: Callable[..., R], *args: object, **kwargs: object) -> R:
    log(f.__name__, args, kwargs)
    return f(*args, **kwargs)
```

### HKT scaffold (v0.5.0)

```python
# Typhon
class Functor[F[_]]:
    ...

def map_through[F[_], A, B](fa: F[A], f: Callable[[A], B]) -> F[B]:
    ...
```

The parser accepts `F[_]` as a type-constructor parameter; full unification is staged. Use conservatively until the frontier work in `TYPE_SYSTEM_FRONTIER.md` lands.

---

## 10. Interfaces

```python
# Typhon
interface Drawable:
    def draw(self) -> None
    def width(self) -> float
    def height(self) -> float

class Button:
    label: str

impl Button:
    def draw(self) -> None: print(self.label)
    def width(self) -> float: return 10.0
    def height(self) -> float: return 1.0

def render(d: Drawable) -> None:
    d.draw()
```

```python
# Emitted Python
from typing import Protocol

class Drawable(Protocol):
    def draw(self) -> None: ...
    def width(self) -> float: ...
    def height(self) -> float: ...

@dataclass(slots=True)
class Button:
    label: str

    def draw(self) -> None:
        print(self.label)
    def width(self) -> float:
        return 10.0
    def height(self) -> float:
        return 1.0

def render(d: Drawable) -> None:
    d.draw()
```

Conformance is verified structurally at the call site. `isinstance(x, Drawable)` is rejected by default (`tyc::interface_isinstance`). Add `@runtime_checkable` to the interface to opt in.

---

## 11. Pipes and guards

```python
# Typhon
let cleaned: str = raw |> str.strip() |> str.lower() |> str.replace(",", "")
let result = x |> normalise() |> scale(2.0) |> clamp(0.0, 1.0)

# Multi-line pipes need wrapping parens:
let final_url: str = (
    raw_url
    |> str.strip()
    |> str.lower()
    |> add_scheme("https://")
)
```

```python
# Emitted Python
cleaned: str = str.replace(str.lower(str.strip(raw)), ",", "")
result = clamp(scale(normalise(x), 2.0), 0.0, 1.0)
final_url: str = add_scheme(str.lower(str.strip(raw_url)), "https://")
```

Left-associative; `a |> f(arg)` is exactly `f(a, arg)`. The piped value fills the *first* positional slot.

```python
# Typhon
def shipping(weight: float?) -> float:
    guard w = weight else:
        return 0.0
    return w * 1.25
```

```python
# Emitted Python
def shipping(weight: float | None) -> float:
    if weight is None:
        return 0.0
    w: float = weight
    return w * 1.25
```

The `guard ... else:` block must return / raise / otherwise leave the enclosing function.

---

## 12. Purity and memo

```python
# Typhon
@pure
def normalise(s: str) -> str:
    return s.strip().lower()

@memo
def fib(n: int) -> int:
    if n < 2: return n
    return fib(n - 1) + fib(n - 2)

@memo(max=128)
def expensive(k: str) -> int: ...

@pure(memo=True)
def hash_pw(salt: str, pw: str) -> str: ...
```

```python
# Emitted Python
import functools

def normalise(s: str) -> str:
    return s.strip().lower()

@functools.cache
def fib(n: int) -> int:
    if n < 2: return n
    return fib(n - 1) + fib(n - 2)

@functools.lru_cache(maxsize=128)
def expensive(k: str) -> int: ...
```

`@pure` alone emits nothing — it's a static assertion. `@memo` (or `@pure(memo=True)`) inserts `functools.cache` / `lru_cache`. Manual `@pure` on a function failing any of the six purity conditions is `tyc::impure_pure_fn`. The verifier is syntactic: it errors on what it can prove impure (I/O, clocks and entropy under any alias, logging, I/O method names, mutation of arguments or module bindings, `mut` reads, non-`@pure` helpers), trusts what it cannot classify, and hands only *provably* pure functions with immutable, hashable parameters and an immutable return type to `auto-memoise` / `pgo-memoise` / `auto-parallel`.

`@gatherable` (no-op at emit) opts a function into auto-gather rewriting under `[strictness] auto-gather = true`. See [SKILL.md](SKILL.md) §10.

---

## 13. `unsafe:` boundary

```python
# Typhon
def parse() -> int:
    unsafe:
        let v = mystery_lib.get_int()
        let checked: int = int(v)
    return checked
```

```python
# Emitted Python
def parse() -> int:
    if True:                       # scope-preserving lowering
        v = mystery_lib.get_int()
        checked: int = int(v)
    return checked
```

The checker tracks an `unsafe_depth` counter, suppresses `tyc::missing_annotation` inside the block, and marks every binding as `Unsafe[T]`. An `Unsafe[T]` cannot cross out of the block into a non-`unsafe` context expecting a concrete `T` — re-assert with an annotated `let`/`mut`, narrow, or cast. Smuggling `Unsafe[T]` outward fires `tyc::unsafe_value_leak`.

Common idiom: end the block with a re-assertion or unreachable raise:

```python
def parse_node(node: object) -> float:
    unsafe:
        if isinstance(node, ast.Constant):
            return float(node.value)
        if isinstance(node, ast.BinOp):
            ...
        raise ValueError(f"unsupported: {type(node).__name__}")
    raise RuntimeError("unreachable")
```

### 13.1 `as!` — checked boundary cast (v0.14.0)

For a single one-off boundary value, `as!` is the lighter, *checked* alternative to an `unsafe:` block:

```python
# Typhon
def load(s: str) -> dict[str, int]:
    let data = json.loads(s) as! dict[str, int]
    return data
```

```python
# Emitted Python
from typhon_runtime.cast import checked_cast as __typhon_checked_cast__

def load(s: str) -> dict[str, int]:
    data = __typhon_checked_cast__(json.loads(s), dict[str, int])
    return data
```

The checker types `EXPR as! TYPE` as `TYPE`, so the boundary value (here `json.loads` → `Any`) flows in without an `unsafe:` block. At runtime `checked_cast` verifies the value's shape against `TYPE` **recursively** and raises `TypeError` on a mismatch — so unlike a static-only re-assertion it actually enforces the claimed type:

```python
load('{"port": 8080}')   # → {'port': 8080}
load('[1, 2, 3]')        # → TypeError: as! cast failed: value of type list does not match dict[str, int]
load('{"port": "x"}')    # → TypeError (dict[str, str] ≠ dict[str, int]; element check recurses)
```

Modelled shapes: scalars (`int` / `str` / `bool` / `float` / `bytes` / `None`), `list[X]` / `set[X]` / `frozenset[X]`, `dict[K, V]`, fixed- and variadic-`tuple[...]`, unions / `Optional`. `int → float` and `bool → int` widening is honoured. `Any` / `object` targets and shapes it can't model fall back to acceptance, so `as!` only ever rejects values it can prove wrong. **Not modelled (origin-only check):** `Callable[...]` signatures, user-defined generic classes (`Box[T]`), and abstract collection types from `collections.abc` / `typing` (`Sequence[X]`, `Iterable[X]`, `Mapping[K, V]`, …). For these the runtime verifies only the erased container/callable origin — `x as! Sequence[int]` confirms `x` is a sequence but **not** that its elements are `int`, and `x as! Callable[[int], int]` confirms `x` is callable but not its signature. Generics are erased in CPython, so this is a hard runtime limitation: when you need element-level guarantees, validate with a `model` class or a concrete `list[int]` / `dict[K, V]` target instead. The VM enforces the same recursive check as the build path (it is no longer an identity pass-through); `tyc fmt` preserves it.

**Scope (v0.15.0):** structural lowering (fixpoint rewrite, bracket-/string-/comment-aware; a quote in a `#` comment no longer derails the scanner, v0.15.2) — composes in any expression position: value positions (`=` / `op=` / `return` / `yield` / bare expression), statement conditions (`if raw as! bool:`), nested inside call arguments (`foo(row[0] as! int, y)`), comprehensions / collection literals, and value expressions spanning multiple physical lines (left operand bracket-balanced). The right operand parses as a type expression (dotted name + optional `[...]` + `|`-union), so trailing code after the type is left outside the cast. (Earlier releases restricted it to a single physical line in value position.)

---

## 14. Source maps

`tyc build` emits `build/*.py` and `build/.sourcemaps/*.py.map` (v0.6.1; legacy `build/*.py.map` location still readable as fallback). The map is **v2**: a per-statement `(out_line → ty_line)` table that:

- `tyc trace` consumes to rewrite Python tracebacks back to `.ty` source
- `tyc debug --break <ty-file>:<line>` consumes to translate `.ty` coordinates to `build/*.py:N` breakpoints
- `tyc debug` source-mapping wrapper (v0.5.0) consumes to overload pdb's `do_list`/`do_where`/`format_stack_entry`/`prompt` so the entire debugger UI reads `.ty`
- `tyc ty` (v0.5.0) consumes to remap `ty`'s `.py:LINE:COL` diagnostics back to `.ty` coordinates
- `tyc lsp` consumes for go-to-definition across the `.ty` ↔ `.py` boundary
- `typhon_runtime/traceback.py` (v0.14.0, when `[emit] traceback-remap = true`) consumes them at **runtime**: the installed `sys.excepthook` reads the sidecars from the running script's `.sourcemaps/` dir and rewrites an uncaught exception's frames to `.ty` — header, source row and all — the `tyc trace` mapping, applied automatically
