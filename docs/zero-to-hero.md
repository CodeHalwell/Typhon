# Typhon: Zero to Hero

A single-sitting path from "never seen Typhon" to "comfortable shipping it." This guide is the express train; the [numbered guides](guides/README.md) are the scenic route with one stop per feature. Where this guide and the [language reference](language.md) ever disagree, the reference wins — and where the reference and `tyc check` disagree, **the compiler wins.** Run it often.

Every code block tagged `python` is Typhon source (a `.ty` file). Most snippets below are lifted straight from the [`examples/`](../examples/) corpus, so they compile as-is.

---

## Table of contents

1. [What Typhon is (and isn't)](#1-what-typhon-is-and-isnt)
2. [Install and your first program](#2-install-and-your-first-program)
3. [The mental model: eight rules](#3-the-mental-model-eight-rules)
4. [Values: `let`, `mut`, and `T?`](#4-values-let-mut-and-t)
5. [Functions](#5-functions)
6. [Collections and control flow](#6-collections-and-control-flow)
7. [Classes, models, and enums](#7-classes-models-and-enums)
8. [Methods live in `impl`](#8-methods-live-in-impl)
9. [Error handling with `Result`](#9-error-handling-with-result)
10. [Sealed unions and exhaustive `match`](#10-sealed-unions-and-exhaustive-match)
11. [Generics and interfaces](#11-generics-and-interfaces)
12. [Domain modelling: `newtype`, `freeze`, `pub`](#12-domain-modelling-newtype-freeze-pub)
13. [Async and concurrency](#13-async-and-concurrency)
14. [The untyped boundary: `unsafe`, `as!`, `.dty`](#14-the-untyped-boundary-unsafe-as-dty)
15. [Build-time power tools: `comptime`, `lazy`, pipes, guards](#15-build-time-power-tools-comptime-lazy-pipes-guards)
16. [How you actually work: the `tyc` workflow](#16-how-you-actually-work-the-tyc-workflow)
17. [Capstone: a small typed program](#17-capstone-a-small-typed-program)
18. [Becoming a hero — where to go next](#18-becoming-a-hero--where-to-go-next)

---

## 1. What Typhon is (and isn't)

Typhon is a **statically-typed, stricter superset of Python that compiles to clean CPython 3.13+.** Think TypeScript, but for Python:

- You write `.ty` files. The compiler, `tyc`, type-checks them and emits ordinary, idiomatic `.py`.
- **Production installs nothing Typhon-specific.** The emitted Python runs on a stock interpreter. Only when you use a handful of features (`Result`, `go`, `freeze let`, …) does the build drop a small self-contained `typhon_runtime/` package next to your output — no PyPI dependency, ever.
- **Not all Python is valid Typhon.** Typhon adds rules (every type annotated, `let`/`mut` on locals, errors as values) that catch a whole class of bugs before runtime.

The entire toolchain — compiler, type checker, formatter, language server, debugger wrapper, REPL, and an in-process interpreter — is **one Rust binary** called `tyc`.

The payoff: Python's ecosystem and readability, with a type system strong enough to make whole categories of `None`-bugs, missing-case bugs, and silent-error bugs *unrepresentable*.

---

## 2. Install and your first program

Install a pre-built binary:

```bash
# macOS / Linux
curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh

# Windows (PowerShell)
iwr -useb https://raw.githubusercontent.com/codehalwell/typhon/main/install.ps1 | iex
```

Or build from source (from the repo root):

```bash
cd tyc && cargo build --release
alias tyc="$PWD/target/release/tyc"
tyc --help
```

Scaffold a project:

```bash
tyc init hello && cd hello
```

You get a `typhon.toml`, a `src/main.ty`, and a `tests/` directory. Here is the canonical first program (`examples/01-hello-world/hello.ty`):

```python
import sys


def main() -> None:
    let name: str = sys.argv[1] if len(sys.argv) > 1 else "world"
    print(f"Hello, {name}!")


if __name__ == "__main__":
    main()
```

Three things to notice already:

- `-> None` is **mandatory**. Typhon has no implicit `Any`; a function that returns nothing says so.
- `let name: str = ...` is an **immutable local binding** with an explicit type. Locals always declare `let` or `mut`.
- `import sys`, f-strings, and `if __name__ == "__main__":` are **unchanged from Python.** Typhon is a superset.

Now the inner loop you'll run a thousand times:

```bash
tyc check src/      # parse + type-check, no files written  (your fast feedback)
tyc run             # execute directly in the in-process VM (no .py emitted)
tyc build           # full pipeline → build/main.py
python build/main.py Alice
# Hello, Alice!
```

`tyc run` executes your program in a built-in tree-walking interpreter — no Python process, no files on disk. `tyc build` emits the `.py` for shipping. The emitted `build/main.py` is byte-identical to the input bar formatting. That's the whole point.

---

## 3. The mental model: eight rules

Typhon looks like Python, but eight rules diverge — and they're the source of every "but this works in Python!" surprise. Internalise these and the rest is detail.

1. **Every parameter and return type is annotated.** No inference fallback. `def f(a, b):` → `tyc::missing_annotation`.
2. **Local bindings declare `let` (immutable) or `mut` (mutable).** A bare `x = 1` inside a function is an error. (Module-level top bindings default to `let`.)
3. **`T` cannot hold `None`.** Nullable is a separate type, `T?`. You must *narrow* before use.
4. **Methods live in `impl` blocks, not in the `class` body.** The constructor is generated — never write `__init__`.
5. **`Any` only enters through an `unsafe:` region or a `.dty` stub.** No accidental dynamic typing.
6. **`match` on a sealed union must be exhaustive.** Miss a variant → compile error.
7. **Errors flow as `Result[T, E]`, not exceptions.** `?` propagates them cleanly.
8. **Declare-only `let NAME: T` must be definitely assigned** on every path before it's read.

The rest of this guide is these eight rules, one at a time, with the syntax that supports them.

---

## 4. Values: `let`, `mut`, and `T?`

### `let` vs `mut`

These govern **binding** immutability — like Rust's `let`/`let mut` or TypeScript's `const`/`let`:

```python
def demo() -> None:
    let pi: float = 3.14159     # immutable binding
    mut counter: int = 0        # mutable binding
    counter = counter + 1       # ✅ reassign a mut
    # pi = 3.14                 # ❌ tyc::immutable_assign
```

**Reach for `let` first.** Switch to `mut` only when you actually rebind. A bare `name = "x"` with no keyword inside a function is `tyc::missing_binding_kind`.

`let` locks the *name*, not the *value* — `let u: User` still allows `u.name = "x"` if the field is mutable. For deep immutability you have `class … frozen:` (instances) and `freeze let` (module constants), both covered later.

### Non-nullable by default

This is the rule that eliminates the billion-dollar mistake. A `str` is *never* `None`. If a value might be absent, its type is `str?` (sugar for `str | None`, emitted exactly that way):

```python
def find(id: int) -> str?:      # may return None
    ...

def greet(name: str) -> None:   # never accepts None
    print(f"Hi {name}")

let found: str? = find(1)
# greet(found)                  # ❌ tyc::nullable_use — found might be None

if found is not None:
    greet(found)                # ✅ narrowed to str inside the branch
```

The checker understands many **narrowing** forms: `is None` / `is not None`, `isinstance`, early-return, exhaustive `match`, ternaries, `and`/`or` short-circuits, and the `guard` statement (Section 15). The cleanest one for "bail if missing":

```python
guard f = find(1) else: return
greet(f)                        # f is str here
```

`dict.get(k)` returns `V?`, not `V` — a common gotcha. Either narrow it or use `d[k]` (typed `V`, may raise `KeyError`).

---

## 5. Functions

Annotate everything. That's Rule 1 and there are no exceptions:

```python
def add(a: int, b: int) -> int:
    return a + b

def log(message: str) -> None:   # returns nothing → -> None
    print(message)
```

Defaults, keyword args, and `*args`/`**kwargs` all work — the variadics just need annotations too (the idiom is `object`):

```python
def connect(host: str, port: int = 8080) -> str:
    return f"{host}:{port}"

def variadic(*args: object, **kwargs: object) -> None:
    ...
```

Generics use PEP 695 syntax (and **only** that — never `from typing import TypeVar`):

```python
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]
```

`first([1, 2, 3])` infers `T = int` and returns `int?`. Inference is bidirectional, so you rarely annotate type arguments by hand.

---

## 6. Collections and control flow

Element types are required — a bare `list` is a missing-annotation error:

```python
let xs: list[int] = [1, 2, 3]
let scores: dict[str, int] = {"alice": 90}
let point: tuple[float, float] = (1.0, 2.0)
let nums: tuple[int, ...] = (1, 2, 3)        # variadic homogeneous tuple
```

Control flow is Python — `if`/`elif`/`else`, `for`, `while`, comprehensions — with the binding rules applied. Loop, `with`, `case`, and `except` *targets* are bindings, not assignments, so they don't take `let`/`mut`:

```python
def total(rows: list[dict[str, int]]) -> int:
    mut sum: int = 0
    for row in rows:                 # `row` is a for-target, no keyword
        sum = sum + row["amount"]
    return sum

let evens: list[int] = [n for n in range(20) if n % 2 == 0]
let lookup: dict[str, int] = {s: len(s) for s in ["a", "bb", "ccc"]}
```

Two quality-of-life diagnostics you'll meet here: a literal `x / 0` is rejected (`tyc::div_by_zero_literal`), and `f = open(path)` without a `with` block warns (`tyc::resource_not_managed`).

---

## 7. Classes, models, and enums

Typhon has several class *forms*, each emitting different Python. Pick by intent:

| Form | Emits | Use for |
|---|---|---|
| `class Foo:` | `@dataclass(slots=True)` | plain value types (the default) |
| `class Foo frozen:` | `@dataclass(slots=True, frozen=True)` | immutable value types |
| `model Foo:` | `class Foo(BaseModel)` + `extra="forbid"` | validated input at a system boundary (Pydantic) |
| `enum Foo:` | `class Foo(enum.Enum)` | a fixed set of named members |
| `interface Foo:` | `class Foo(Protocol)` | structural contracts (Section 11) |
| `plain class Foo:` | bare `class Foo:` | metaclass-driven / ORM libraries |
| `class! Foo(Base):` | bare `class Foo(Base):` + synthesised `__init__` | subclassing a framework base (e.g. `nn.Module`) |

A plain value type — note there is **no `__init__`**; it's generated, and writing one is an error:

```python
class User:
    id: int
    name: str

let alice: User = User(id=1, name="Alice")
print(alice.name)
```

A `model` validates at the boundary (great for parsing untrusted JSON, config, request bodies):

```python
model ApiUser:
    id: int
    name: str
    email: str?              # optional field
```

And `enum` (sugar over `enum.Enum`) — from `examples/49-enums/enums.ty`:

```python
enum Direction:
    NORTH
    EAST
    SOUTH
    WEST

enum Priority:
    LOW = 10
    MEDIUM = 20
    HIGH = 30
    URGENT                   # resumes auto-numbering at 31
```

Bare members auto-number via `enum.auto()`; explicit values are preserved. Members repr as `Direction.NORTH`, iterate in declaration order, and round-trip through the constructor (`Priority(30)` is `HIGH`).

One ordering rule the compiler enforces (because `@dataclass` would raise at import otherwise): a non-defaulted field can't follow a defaulted one — `tyc::field_default_ordering`.

---

## 8. Methods live in `impl`

This is Rule 4, and it's the biggest visual departure from Python. The `class` body holds **fields only**; methods go in a separate `impl` block:

```python
class User:
    id: int
    name: str

impl User:
    def display(self) -> str:        # explicit self; fields via self.NAME
        return f"{self.name} (#{self.id})"

    def with_name(self, name: str) -> User:
        return User(id=self.id, name=name)
```

`impl` and `class` can even live in different files. The sibling keyword `extend` does the same thing across modules (and `extend str:` / `extend list:` can add static methods to built-ins). At emit time, `impl`/`extend` methods are merged back into the class body — the generated Python looks exactly like a normal class with methods.

Why split them? It keeps the data shape declarative, makes cross-module extension uniform, and — crucially — lets you attach methods to a *sealed union* and have them distributed to every variant (Section 10).

---

## 9. Error handling with `Result`

Rule 7. Expected failures are **values**, not exceptions. `Result[T, E]` is a sealed type with two cases, `Ok(value)` and `Err(error)`. Here's the canonical example (`examples/07-error-handling/error_handling.ty`):

```python
class ParseError:
    field: str
    reason: str


def parse_port(raw: str) -> Result[int, ParseError]:
    if not raw.isdigit():
        return Err(ParseError(field="port", reason=f"not a number: {raw}"))
    let n: int = int(raw)
    if n < 1 or n > 65535:
        return Err(ParseError(field="port", reason=f"out of range: {n}"))
    return Ok(n)
```

### The `?` operator

`?` unwraps an `Ok` and short-circuits an `Err` out of the current function. It is **not** `try/except` — it desugars to a plain `isinstance` check and early `return`, so stack traces stay clean:

```python
def parse_addr_short(host_raw: str, port_raw: str) -> Result[tuple[str, int], ParseError]:
    let host: str = parse_host(host_raw)?     # unwrap Ok, or return the Err
    let port: int = parse_port(port_raw)?
    return Ok((host, port))
```

`?` only works inside a function that itself returns a compatible `Result` (else `tyc::invalid_question_op`), and the error types must line up (else `tyc::result_error_mismatch`).

### `with`-chains

For several dependent steps with shared error handling:

```python
def parse_addr(host_raw: str, port_raw: str) -> Result[tuple[str, int], ParseError]:
    with host = parse_host(host_raw)?,
         port = parse_port(port_raw)?:
        return Ok((host, port))
    else err:
        print(f"failed parsing {err.field}: {err.reason}")
        return Err(err)
```

### Consuming a `Result`

You `match` on it (note: no `_` fallthrough needed — the two cases are exhaustive):

```python
match parse_addr("localhost", "8080"):
    case Ok((host, port)):
        print(f"bound to {host}:{port}")
    case Err(e):
        print(f"failed: {e.reason}")
```

Combinators (`.map`, `.map_err`, `.and_then`, `.or_else`) chain transformations without unwrapping. And to bridge a library that *throws*, wrap it once at the boundary — either a small `try` shim, or the one-expression `try_result`:

```python
import json

def parse_json(text: str) -> Result[dict[str, object], str]:
    return try_result(lambda: json.loads(text), lambda e: f"invalid JSON: {e}")
```

After that boundary, everything downstream uses `?` and never sees an exception.

---

## 10. Sealed unions and exhaustive `match`

A sealed union is a closed set of variants — the foundation for modelling "one of these N shapes." From `examples/08-sealed-unions-match/sealed_unions.ty`:

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
        case Circle(radius):
            return 3.14159 * radius * radius
        case Rectangle(width, height):
            return width * height
        case Triangle(base, height):
            return 0.5 * base * height
```

The magic is Rule 6: **the `match` is statically checked exhaustive.** No `case _:` needed. Now add a `Pentagon` variant to the `type Shape = ...` alias and *every* match over `Shape` lights up with `tyc::non_exhaustive_match` until you handle it. That's how you make "I forgot a case" a compile error instead of a 2am page.

You can attach behaviour to the whole union with `impl`, which distributes the method to every variant:

```python
impl Shape:
    def is_round(self) -> bool:
        match self:
            case Circle(_):              return True
            case Rectangle(_, _):        return False
            case Triangle(_, _):         return False
```

> Sealed unions vs inheritance: prefer unions for *closed* sets of variants — they give you exhaustiveness; subclassing doesn't. Use inheritance for *open* extension.

---

## 11. Generics and interfaces

Generics are PEP 695, on functions, classes, and aliases:

```python
class Box[T]:
    value: T

impl[T] Box[T]:
    def map[U](self, f: Callable[[T], U]) -> Box[U]:
        return Box(value=f(self.value))

type Pair[A, B] = tuple[A, B]
```

Interfaces are **structural** contracts — Python `Protocol`s under the hood. A type conforms by having the right methods, no declaration needed:

```python
interface Drawable:
    def draw(self) -> None
    def width(self) -> float

class Button:
    label: str

impl Button:
    def draw(self) -> None:
        print(self.label)
    def width(self) -> float:
        return float(len(self.label) + 4)

def render(d: Drawable) -> None:
    d.draw()

render(Button(label="x"))        # ✅ Button structurally matches Drawable
```

One sharp edge: `isinstance(x, Drawable)` is **rejected** (`tyc::interface_isinstance`) because Python's runtime protocol check only verifies attribute *presence*, not signatures. Narrow statically or use a sealed union instead.

---

## 12. Domain modelling: `newtype`, `freeze`, `pub`

These three turn "it's just an `int`" bugs into compile errors and make your module boundaries explicit.

### `newtype` — nominal IDs

A `newtype` is a zero-cost distinct type over a base. From `examples/48-newtype-ids/newtype_ids.ty`:

```python
newtype UserId = int
newtype PostId = int
newtype Email = str

def greet(uid: UserId, email: Email) -> str:
    return f"hi user#{uid} ({email})"

let me: UserId = UserId(7)
greet(me, Email("ada@example.com"))     # ✅
# greet(42, ...)                        # ❌ tyc::newtype_violation — wrap as UserId(42)
```

The relationship is **asymmetric**: a `UserId` flows freely into an `int` slot (it really *is* an `int` at runtime), but a bare `int` won't satisfy a `UserId` parameter without the explicit constructor. So you can never accidentally pass a `PostId` where a `UserId` belongs — even though both are `int`.

### `freeze let` — deep-immutable constants

`let` locks the binding; `freeze let` recursively freezes the *value* too (module level):

```python
freeze let CONFIG = {"port": 8080, "hosts": ["a", "b"]}
# CONFIG["port"] = 9000      # ❌ TypeError at runtime — it's a read-only mapping
# CONFIG["hosts"].append(…)  # ❌ the list became a tuple
```

It converts `list → tuple`, `dict → read-only mapping`, `set → frozenset`, all the way down.

### `pub` — module visibility

Mark the public surface of a module with `pub`; everything else stays internal:

```python
pub let API_VERSION: str = "v1"
pub class Client:
    host: str
pub def connect(host: str) -> Client: ...

let _default_port: int = 8080      # not exported
```

When a module has any `pub` names, an `__all__` is synthesised for you. In an `__init__.ty`, `pub *` aggregates every sibling module's public surface into the package — the clean way to build a multi-file library.

---

## 13. Async and concurrency

`async`/`await` work as in Python, but concurrency is *explicit* — calling an `async def` from a sync context without `await` is a hard error (`tyc::missing_await`), and blocking calls (`time.sleep`, `requests.get`) inside `async def` get flagged (`tyc::blocking_in_async`).

The headline feature is `gather:` — run independent awaits concurrently with clean syntax:

```python
async def load(uid: int) -> Dashboard:
    gather:
        user   = fetch_user(uid)
        posts  = fetch_posts(uid)
        notifs = fetch_notifs(uid)
    return Dashboard(user=user, posts=posts, notifs=notifs)
```

This lowers to an `asyncio.TaskGroup` (cancel-on-failure). The bindings inside `gather:` don't need `let`/`mut` — the keyword introduces them. If one binding depends on another, the block gracefully degrades to sequential awaits. For "I don't care about failures individually," use `gather(strategy="best-effort"):` (lowers to `asyncio.gather(return_exceptions=True)`).

For fire-and-forget, use `go` (never bare `asyncio.create_task` — Python's loop holds only weak refs and can GC your task mid-flight; `go` registers a strong ref):

```python
async def signup(email: str) -> User:
    let user: User = await create(email)
    go send_welcome(user)        # fire-and-forget, kept alive by the runtime
    return user
```

---

## 14. The untyped boundary: `unsafe`, `as!`, `.dty`

Rule 5: `Any` doesn't leak in by accident. When you call an untyped library or parse a raw payload, you cross a boundary deliberately. You have three tools, in increasing permanence:

### `as!` — checked one-off cast (the everyday tool)

`EXPR as! TYPE` types the expression as `TYPE` **and** lowers to a runtime structural check, so it can only let through values it can't prove wrong. Perfect for a JSON field or a DB row. From `examples/59-boundary-casts/boundary_casts.ty`:

```python
def read_service(text: str) -> Result[str, str]:
    let raw: dict[str, object] = parse_json(text)?
    let name: str = raw["name"] as! str
    let port: int = raw["port"] as! int
    let hosts: list[str] = raw["hosts"] as! list[str]
    let limits: dict[str, int] = raw["limits"] as! dict[str, int]
    return Ok(describe(name, port, hosts, limits["rps"]))
```

Unlike TypeScript's unchecked `as`, an `as!` that can't be proven correct **raises at runtime** — so wrap it in `try_result` and a bad payload becomes a clean `Err` instead of a mislabelled value flowing downstream.

### `unsafe:` — a lexical region

For a cluster of dynamic calls, an `unsafe:` block lets `Any`-inferring expressions bind freely. You must re-assert a concrete type before a value crosses *out* of the block:

```python
def parse() -> int:
    unsafe:
        let v = mystery_lib.get_int()
        let checked: int = int(v)        # re-assert at the boundary
    return checked
```

### `.dty` stubs — for long-lived dependencies

For a library you call all over the place, write a `.dty` stub once — strictly-typed Typhon describing its API. The compiler also ships bundled stubs for popular libraries (`httpx`, `requests`) and can introspect installed packages' signatures automatically, so a wrong-typed argument to a third-party function is often caught with zero authoring.

---

## 15. Build-time power tools: `comptime`, `lazy`, pipes, guards

A grab-bag of features that pay off once the basics are second nature.

**`comptime`** evaluates at *build time* and inlines the result as a literal — config-from-env without runtime cost:

```python
comptime let PORT: int = int(env("PORT", "8080"))
comptime let IS_PROD: bool = env("BUILD_TAG", "dev") == "prod"
```

The sandbox is hermetic (no I/O, no loops, no imports) — pass everything in. Don't put secrets here; they'd be baked into the artifact (`tyc::contains_secret_literal` warns you).

**`lazy import`** defers an expensive module until first use:

```python
lazy import np = numpy           # numpy isn't imported until you touch `np`
```

(`lazy from numpy import array` is rejected — it defeats the deferral.)

**Pipes** read left-to-right instead of inside-out:

```python
let result = data |> clean() |> normalize() |> summarize()
# same as summarize(normalize(clean(data)))
```

**`guard`** is early-return narrowing — bail cleanly when a value is missing:

```python
def first_name(user: User?) -> str:
    guard u = user else: return "anonymous"
    return u.name            # u is User here
```

Also in the box: `@pure` (assert a function is side-effect-free) and `@memo` (`@functools.cache`).

---

## 16. How you actually work: the `tyc` workflow

Your daily commands:

| Command | What it does | When |
|---|---|---|
| `tyc check src/` | parse + type-check, no output | constantly — your fast feedback loop, and the CI gate |
| `tyc run` | execute in the in-process VM | iterating on pure-Typhon logic |
| `tyc build` | full pipeline → `build/*.py` | producing shippable Python |
| `tyc fmt src/` | format (whitespace + `ruff format`) | pre-commit |
| `tyc explain <code>` | explain a `tyc::` diagnostic offline | when an error needs context |
| `tyc cheatsheet` | the 30-second syntax table | memory jog |
| `tyc migrate app.py` | typed Python → Typhon | porting an existing file |
| `tyc repl` | interactive evaluator | quick experiments |

`tyc run` uses an in-process interpreter — fast, zero files. It covers a large surface but if your program imports CPython-only libraries, fall back to `tyc run --compile` (build then exec).

The golden rule: **when in doubt, run `tyc check`.** Every error names a `tyc::` code; `tyc explain <code>` tells you exactly what it means and how to fix it. The compiler is the source of truth for what the language accepts today.

### The diagnostics you'll hit first

| Code | Meaning | Fix |
|---|---|---|
| `tyc::missing_annotation` | unannotated param or return | add the type (`-> None` if it returns nothing) |
| `tyc::missing_binding_kind` | bare `=` local | add `let` or `mut` |
| `tyc::immutable_assign` | reassigned a `let` | use `mut` |
| `tyc::nullable_use` | used a `T?` where `T` required | narrow it first |
| `tyc::non_exhaustive_match` | a sealed-union case is missing | add the `case` |
| `tyc::manual_init` | wrote `__init__` | delete it — it's generated |
| `tyc::method_in_class_body` | method in `class` not `impl` | move it to an `impl` block |
| `tyc::invalid_question_op` | `?` outside a `Result` function | fix the signature or `match` |

---

## 17. Capstone: a small typed program

Let's combine the pieces — `newtype` IDs, a `model` boundary, `Result`/`?`, a sealed union, and exhaustive `match` — into one coherent program. Save as `src/main.ty` and run `tyc run`.

```python
import json

newtype UserId = int


# A validated boundary type. Parsing untrusted input goes through `model`.
model SignupRequest:
    name: str
    age: int


# A closed set of outcomes — exhaustively matchable.
type Outcome = Accepted | Rejected

class Accepted:
    user_id: UserId
    name: str

class Rejected frozen:
    reason: str


def parse_json(text: str) -> Result[dict[str, object], str]:
    return try_result(lambda: json.loads(text), lambda e: f"bad JSON: {e}")


def parse_request(text: str) -> Result[SignupRequest, str]:
    let raw: dict[str, object] = parse_json(text)?
    let name: str = raw["name"] as! str
    let age: int = raw["age"] as! int
    return Ok(SignupRequest(name=name, age=age))


def decide(req: SignupRequest, next_id: UserId) -> Outcome:
    if req.age < 18:
        return Rejected(reason=f"{req.name} is under 18")
    return Accepted(user_id=next_id, name=req.name)


def render(outcome: Outcome) -> str:
    match outcome:
        case Accepted(user_id, name):
            return f"welcome {name}, you are user #{user_id}"
        case Rejected(reason):
            return f"sorry: {reason}"


def main() -> None:
    let inputs: list[str] = [
        '{"name": "Ada", "age": 31}',
        '{"name": "Kid", "age": 12}',
        "not json at all",
    ]
    mut next_id: int = 100
    for text in inputs:
        match parse_request(text):
            case Ok(req):
                let outcome: Outcome = decide(req, UserId(next_id))
                print(render(outcome))
                next_id = next_id + 1
            case Err(msg):
                print(f"dropped input: {msg}")


if __name__ == "__main__":
    main()
```

Walk through what each rule bought you:

- **`newtype UserId`** means a raw `int` can't be passed where a user id belongs — the `UserId(next_id)` wrap is the only way in.
- **`model SignupRequest`** validates the shape at the boundary; **`as!`** checks each untyped JSON field at runtime.
- **`Result` + `?`** make the "bad input" path explicit and impossible to forget — a malformed line becomes an `Err`, not a crash.
- **`type Outcome = Accepted | Rejected`** plus the `match` in `render` means adding a third outcome later forces you to handle it everywhere.

That's the Typhon value proposition in 50 lines: the type system did the bookkeeping so the bugs never compiled.

---

## 18. Becoming a hero — where to go next

You now know enough to write real Typhon. To go deep:

- **The numbered guides** ([`docs/guides/`](guides/README.md)) — one focused chapter per feature, each with the emitted Python and the diagnostics you'll see. Read them in order to fill gaps.
- **The example corpus** ([`examples/`](../examples/)) — 30-odd runnable exercises (`01`–`68`) plus 15 production-shaped multi-file apps under [`examples/apps/`](../examples/apps/) (event-sourced banking, a mini-compiler, a search engine, a trading engine, …). Read them, run them, modify them.
- **The language reference** ([`docs/language.md`](language.md)) — the canonical spec for the type system, error handling, async, and `comptime`.
- **The cheatsheet** ([`docs/cheatsheet.md`](cheatsheet.md) or `tyc cheatsheet`) — the whole syntax on one screen.
- **The CLI reference** ([`docs/cli.md`](cli.md)) — every `tyc` subcommand and flag.
- **`tyc explain <code>`** — your in-terminal tutor for any diagnostic.

The fastest way to mastery is the tightest loop: write a little, `tyc check`, read the diagnostic, fix it, repeat. The compiler is patient and it's always right. Welcome to Typhon.
```
