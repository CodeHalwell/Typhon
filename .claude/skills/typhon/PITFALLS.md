# Typhon — Extended Pitfalls Catalogue

The errors and surprises that bite people who try to write Typhon as if it were Python. Ranked roughly by frequency, with the diagnostic you'll see and the fix.

For each entry: **trigger → diagnostic → fix**.

Current release: **v0.14.0**. Pitfalls tagged with a version annotation landed in that release. Pitfalls 61–75 are the v0.9.0 cleanup additions covering the daily-driver VM, type-checker covariance and narrowing gaps, and multi-file project support; pitfalls 76+ cover the v0.10.0–v0.12.0 VM-completeness, `enum`, and third-party-type-checking surface; pitfalls 81–82 cover the v0.14.0 `as!` checked boundary cast.

---

## 1. Forgetting the return type

```python
def main():
    print("hi")
```

```
error[tyc::missing_annotation]: function `main` is missing a return type
 ┌─ src/main.ty:1:5
 │
1 │ def main():
 │     ^^^^ add `-> None` (or another type) here
```

**Fix:** `def main() -> None:`.

`[strictness] no-implicit-any = true` is the default. Even functions that return nothing need `-> None` — there is no inference fallback.

---

## 2. Function-local `=` without `let`/`mut`

```python
def main() -> None:
    name = "Alice"
    print(name)
```

```
error[tyc::missing_binding_kind]: local bindings must be declared with `let` or `mut`
 ┌─ src/main.ty:2:5
 │
2 │     name = "Alice"
 │     ^^^^ add `let` (immutable) or `mut` (mutable) here
```

**Fix:** `let name: str = "Alice"` or `mut name: str = "Alice"`.

Module-level annotated assignments are fine without the keyword (they default to `let`). The rule is for *function-local* bindings only.

---

## 3. Reassigning a `let`

```python
def demo() -> None:
    let count: int = 0
    count = count + 1
```

```
error[tyc::immutable_assign]: cannot reassign `let` binding `count`
 ┌─ src/main.ty:3:5
 │
3 │     count = count + 1
 │     ^^^^^ declare with `mut` to allow reassignment
```

**Fix:** `mut count: int = 0`.

---

## 4. Passing `T?` where `T` is required

```python
def greet(name: str) -> None: ...
def find(id: int) -> str?: ...

let raw: str? = find(1)
greet(raw)
```

```
error[tyc::nullable_use]: cannot pass `str | None` where `str` is required
 ┌─ src/main.ty:5:7
```

**Fix:** narrow first.

```python
if raw is not None:
    greet(raw)

# or
guard r = raw else: return
greet(r)
```

---

## 5. Putting methods inside `class`

```python
class User:
    id: int

    def display(self) -> str:
        return f"user {self.id}"
```

`class` declarations are minimalist: no `__init__`, no methods. Methods live in `impl`:

```python
class User:
    id: int

impl User:
    def display(self) -> str:
        return f"user {self.id}"
```

The desugarer merges the `impl` block in. Severity controlled by `[strictness] methods-in-class-body` (default `"warn"`; bump to `"error"` for CI).

---

## 6. Writing `__init__`

```python
class User:
    id: int

    def __init__(self, id: int) -> None:
        self.id = id
```

```
error[tyc::manual_init]: classes do not declare `__init__`; the constructor is generated
```

**Fix:** drop it. Use field defaults for "convenience constructors", or write a free function (`def make_user(...) -> User: ...`).

---

## 7. Using `TypeVar` instead of PEP 695

```python
from typing import TypeVar
T = TypeVar("T")
def first(xs: list[T]) -> T | None: ...
```

Use PEP 695 syntax:

```python
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]
```

Typhon does not accept the `TypeVar` import path (`tyc::typevar_import_rejected`). Same for deprecated capitalised typing aliases: `from typing import List/Dict/Tuple/Set/FrozenSet/Type` → `tyc::typing_alias_deprecated`. Use lowercase built-ins.

---

## 8. `isinstance` against an interface

```python
interface Drawable:
    def draw(self) -> None

def render_if_able(x: object) -> None:
    if isinstance(x, Drawable):
        x.draw()
```

```
error[tyc::interface_isinstance]: runtime `isinstance` against an interface is unsafe
                                  by default; use static narrowing or opt into
                                  `@runtime_checkable` explicitly
```

Python's `@runtime_checkable` only checks attribute *presence*, not signatures — so an object with a `draw` field that isn't callable would pass. Typhon refuses by default.

**Fix options:**

- Refactor to a sealed union if the variants are closed.
- Write an explicit predicate function with hasattr+callable checks.
- Static narrowing via a parameter typed as the interface.
- Decorate the interface `@runtime_checkable` to opt in (then `isinstance` is permitted, with the same Python-level weakness).

---

## 9. Using `asyncio.create_task` directly

```python
asyncio.create_task(send_welcome(user))
```

Python's event loop holds **weak** refs to tasks. A fire-and-forget task whose handle is dropped can be GC'd mid-flight.

**Fix:** `go send_welcome(user)`. Lowers through `typhon_runtime.tasks.spawn`, which keeps a strong-ref registry.

---

## 10. `lazy from foo import bar`

```python
lazy from numpy import array
```

```
error[tyc::lazy_usage]: `lazy from ... import ...` is rejected — `from` imports eagerly
                       touch attributes on the source module and defeat deferral
```

**Fix:** `lazy import np = numpy`, then `np.array(...)`.

---

## 11. `comptime` reading runtime-only values

```python
comptime let NOW: float = time.time()
```

```
error[tyc::comptime]: `comptime` sandbox forbids `time.time`
                      (non-deterministic clock)
```

The comptime sandbox allows: pure arithmetic, string ops (incl. `str.join` v0.3.1), `env(name, default?)`, simple containers, ternaries, `if`/`elif`/`else`, types-as-values (v0.5.0), calls to other comptime functions. It forbids: I/O, subprocess, network, random, time, uuid, arbitrary imports, loops, exceptions, `with` blocks, classes, free variables, `*args`/`**kwargs`/defaults.

**Fix:** compute at runtime, cache with `lazy let`:

```python
lazy let STARTUP_TIME: float = time.time()
```

---

## 12. Missing variants in a sealed-union `match`

```python
type Shape = Circle | Rectangle | Triangle

def area(s: Shape) -> float:
    match s:
        case Circle(r):       return 3.14159 * r * r
        case Rectangle(w, h): return w * h
        # forgot Triangle
```

```
error[tyc::non_exhaustive_match]: match does not cover variant `Triangle`
```

**Fix:** add the missing case, or use `case _:` if a deliberate catch-all is the intent. Using `case _:` opts out of exhaustiveness for the rest of the union — use sparingly. Severity controlled by `[strictness] exhaustive-match` (default `"error"`).

---

## 13. `?` outside a `Result`-returning function

```python
def main() -> None:
    let port: int = parse_port("8080")?
```

```
error[tyc::invalid_question_op]: `?` can only short-circuit inside a function
                                              returning a compatible `Result`
```

**Fix:** change the signature (`-> Result[int, str]`) or `match` explicitly.

`?` inside a comprehension is also rejected (v0.3.1) with a dedicated message — rebind the result and unwrap it separately.

---

## 14. Mismatched error types across `?`

```python
def parse_port(raw: str) -> Result[int, str]: ...

def boot(raw: str) -> Result[int, ConfigError]:
    let port: int = parse_port(raw)?
```

```
error[tyc::result_error_mismatch]: cannot propagate `Err[str]` into a function
                                   returning `Result[int, ConfigError]`
```

**Fix:** convert at the boundary, either with `match`:

```python
match parse_port(raw):
    case Ok(port):
        return Ok(port)
    case Err(msg):
        return Err(BadPort(raw=raw))
```

…or with `.map_err` (v0.6.0):

```python
let port: int = parse_port(raw).map_err(lambda msg: BadPort(raw=raw))?
```

---

## 15. `@pure` on an impure function

```python
@pure
def fetch(url: str) -> str:
    import urllib.request
    return urllib.request.urlopen(url).read().decode()
```

```
error[tyc::impure_pure_fn]: `fetch` is annotated `@pure` but performs I/O
                            (urllib.request.urlopen)
```

The six purity conditions: synchronous, hashable params, no I/O, no non-determinism, no mutable module state, no exceptions raised. Failing *any* condition is a hard error.

**Fix:** drop `@pure` (and lose memo eligibility), or refactor the I/O out.

---

## 16. Bare `list` / `dict` annotation

```python
let xs: list = [1, 2, 3]
```

```
error[tyc::missing_annotation]: list element type is required; provide a parameter
```

**Fix:** `let xs: list[int] = [1, 2, 3]`.

Same for `dict`, `set`, `tuple`. Element / value types are mandatory under `[strictness] no-implicit-any = true` (the default).

---

## 17. `dict.get(k)` as if it returned `V`

```python
let counts: dict[str, int] = {"a": 1}
let n: int = counts.get("missing")
```

`get(...)` returns `V?`. Either narrow:

```python
let n: int? = counts.get("missing")
if n is None:
    return
print(n)
```

…or use `d[k]` (typed `V`, may raise `KeyError`).

---

## 18. `bool` treated as if NOT `int` (v0.4.0 update)

Since v0.4.0, **`bool ⊆ int`** is one-way: `let x: int = True` type-checks; `1 + True` type-checks; `-True` type-checks. **The reverse is still rejected:**

```python
def takes_int(n: int) -> None: ...
let flag: bool = True
takes_int(flag)               # ✅ now type-checks (v0.4.0)

def takes_bool(b: bool) -> None: ...
let n: int = 1
takes_bool(n)                 # ❌ tyc::type_mismatch — bool is NOT a supertype of int
```

**Fix on the second line:** `takes_bool(bool(n))`.

---

## 19. Mismatched default arg

```python
def bad(n: int = "zero") -> int: ...
```

```
error[tyc::default_mismatch]: default value type `str` does not match annotation `int`
```

**Fix:** match the type, or change the annotation.

---

## 20. `Unsafe[T]` leaking out of `unsafe:`

```python
def parse() -> int:
    unsafe:
        let v = mystery_lib.get_int()
    return v
```

```
error[tyc::unsafe_value_leak]: cannot return `Unsafe[Any]` from a function declared `-> int`
```

`Unsafe[T]` is a hidden marker — visible in diagnostics, not in source. Re-assert at the boundary:

```python
def parse() -> int:
    unsafe:
        let v = mystery_lib.get_int()
        let checked: int = int(v)
    return checked
```

Or end the block with an unreachable raise:

```python
def parse_node(node: object) -> float:
    unsafe:
        if isinstance(node, ast.Constant):
            return float(node.value)
        raise ValueError(f"unsupported: {type(node).__name__}")
    raise RuntimeError("unreachable")
```

---

## 21. Empty list with unconstrained generic

```python
let empty = first([])
```

```
error[tyc::missing_annotation]: cannot infer `T` for `first` — argument has no element type
```

**Fix:** annotate at the call: `let empty: int? = first[int]([])`, or `let empty: int? = first([] : list[int])`.

---

## 22. `async def` with no `await`

```python
async def helper() -> int:
    return 42
```

```
warning[tyc::async_without_await]: `helper` is `async` but never awaits;
                                   make it sync, or `await` something inside it
```

Almost always a mistake. Drop the `async` or `await` something. Warning-level by default.

---

## 23. Forgetting `await`

```python
async def fetch(url: str) -> str: ...

def main() -> None:
    let body: str = fetch("...")
```

```
error[tyc::missing_await]: cannot use a coroutine where `str` is required
```

**Fix:** `let body: str = await fetch("...")` and make `main` `async`. Or at the top level, `asyncio.run(fetch(...))` — that path is whitelisted (v0.6.0).

---

## 24. Blocking I/O inside `async def`

```python
import time
import requests

async def fetch(url: str) -> bytes:
    time.sleep(1)
    return requests.get(url).content
```

```
warning[tyc::blocking_in_async]: `time.sleep` blocks the event loop
```

The check covers `time.sleep`, `requests.{get,post,...}`, `socket.recv`, `subprocess.{run,call,check_call,check_output}`, `input`, `urllib.request.urlopen`. Suppressed inside `unsafe:`. Severity controlled by `[strictness] blocking-in-async`.

**Fix:** wrap with `asyncio.to_thread(...)`, use an async-native client (`aiohttp`, `httpx.AsyncClient`, `asyncio.sleep`), or move the call out of the coroutine.

---

## 25. Mistaking dataclass `frozen=True` for deep immutability

```python
class Box frozen:
    items: list[int]

let b: Box = Box(items=[1, 2, 3])
b.items.append(4)        # ✅ allowed — nested list is mutable
b.items = []             # ❌ tyc::frozen_assign — field reassignment blocked
```

Dataclass `frozen=True` blocks **field reassignment**, not nested mutation. For deep immutability, use immutable containers inside (`tuple`, `frozenset`) — or `freeze let` at the module level (v0.3.0).

---

## 26. Stub drift between `.dty` and runtime

```
error[tyc::stub_mismatch]: stub declares `Redis.set(key, value) -> bool` but runtime
                          signature is `set(name, value, ex=None) -> bool`
```

Raised by `tyc check --stubs`. For in-tree stubs (you wrote both), it's a code-review fail. For third-party stubs, it usually means the library was upgraded — update the stub and re-check.

Also catchable with `tyc stubtest` (runs `python -m mypy.stubtest` against emitted `.pyi`s; catches dynamically-created members the AST can't see).

---

## 27. `with`-chain in a non-`Result` function

```python
def main() -> None:
    with user = db.find(1)?, perms = check(user)?:
        return Ok(...)
    else err:
        return Err(err)
```

```
error[tyc::invalid_question_op]: `?` inside a `with`-chain requires
                                              an enclosing `Result`-returning function
```

**Fix:** lift to a helper that returns `Result`, or write the chain without `?` and `match` each step.

---

## 28. `gather:` with dependent values

```python
gather:
    user = fetch_user(uid)
    posts = fetch_posts(user.id)   # depends on `user`
```

Dependent bindings gracefully degrade to sequential `await` in source order — you won't get a diagnostic, but you also won't get concurrency for that step. The right shape is to fetch the dependency first, then `gather:` the independents:

```python
let user: User = await fetch_user(uid)
gather:
    posts = fetch_posts(user.id)
    notifs = fetch_notifs(user.id)
```

---

## 29. Module-level `lazy let` consumed inside a hot loop

```python
lazy let CONFIG: Config = load_config_from_disk()

def serve(n: int) -> None:
    for _ in range(n):
        if CONFIG.feature_x:
            ...
```

The first access pays the load cost; subsequent accesses are memory reads — but the runtime helper still goes through a small `__call__` shim. For very hot inner loops, hoist:

```python
def serve(n: int) -> None:
    let feature_x: bool = CONFIG.feature_x
    for _ in range(n):
        if feature_x:
            ...
```

Not a correctness issue — a perf hint.

---

## 30. Bare `open(...)` outside a `with` block (v0.3.0)

```python
def load(path: str) -> dict[str, int]:
    let f = open(path)
    return json.load(f)
```

```
warning[tyc::resource_not_managed]: `open(...)` result is not wrapped in `with`
                                      = help: deterministic cleanup matters for file handles
```

Covers `open`, `socket.socket`, `sqlite3.connect`, `tempfile.NamedTemporaryFile`, `tempfile.TemporaryDirectory`, `tempfile.TemporaryFile`. Severity is `warn` by default; bump to `error` via `[strictness] require-with = "error"`. `@contextmanager` / `@asynccontextmanager` factory bodies are exempt (v0.6.0).

**Fix:** rewrite as `with open(path) as f:`, or accept the warning if you're managing cleanup explicitly (e.g. handing the handle off to another function that closes it).

---

## 31. Bare `int` flowing into a `newtype` slot (v0.3.0)

```python
newtype UserId = int

def fetch_user(id: UserId) -> User: ...

fetch_user(42)
```

```
error[tyc::newtype_violation]: `int` is not assignable to `UserId`
```

**Fix:** `fetch_user(UserId(42))`. The reverse direction (`let raw: int = uid` where `uid: UserId`) is allowed — that's the asymmetric-by-design part of `newtype`.

If you find yourself wrapping at every call site, the boundary is in the wrong place: either lift the wrap into the function that produces the value, or relax the parameter type back to `int`.

### 31a. Cross-newtype arithmetic (v0.7.0)

```python
newtype LogIndex = int
newtype Term = int

let li: LogIndex = LogIndex(5)
let t: Term = Term(2)
let bad: LogIndex = li + t       # ❌ tyc::operator_type_mismatch
```

Same-newtype arithmetic preserves the newtype across `+ - * // % **`; two distinct newtypes don't unify even when they share a base. Wrap explicitly: `LogIndex(li + LogIndex(t))` or convert: `li + LogIndex(int(t))`.

---

## 32. `case value:` shadowing an outer `let value` (v0.3.0)

```python
let value: int = 1
match thing:
    case Wrap(value):              # bare name → fresh binding in match patterns
        print(value)
```

```
error[tyc::pattern_shadows_outer]: pattern capture `value` shadows outer `let value`
```

This used to fire as the misleading `tyc::immutable_assign`; v0.3.0 catches the case explicitly.

**Fix:** rename the capture (`case Wrap(inner): ...`). The Python `match` spec defines bare names in class patterns as *fresh* bindings, not references to outer variables — this is one of the cases where the syntax doesn't match the Rust/OCaml/Scala intuition.

---

## 33. Literal divide-by-zero (v0.3.0)

```python
def half(x: int) -> float:
    return x / 0
```

```
error[tyc::div_by_zero_literal]: division by literal zero always raises
```

Catches `/`, `//`, `%` against `0`, `0.0`, `-0`, `-0.0`, and any unary-negated zero. Pure constant-fold; flow-sensitive analysis (`if d != 0:` guards on runtime values) is deliberately out of scope.

**Fix:** the divisor is the bug — there is no "right answer" to fish back. If you're testing the error path itself, wrap in `unsafe:`.

---

## 34. Mutating a `freeze let` value (v0.3.0)

```python
freeze let CONFIG = {"port": 8080, "hosts": ["a", "b"]}
CONFIG["port"] = 9000
CONFIG["hosts"].append("c")
```

Both runtime errors. `freeze let` lowers through `typhon_runtime.freeze.deep_freeze`, which converts `dict → MappingProxyType`, `list → tuple`, `set → frozenset` recursively. The first line raises `TypeError: 'mappingproxy' object does not support item assignment`; the second raises `AttributeError: 'tuple' object has no attribute 'append'`.

**Fix:** if you need to "mutate", build a new frozen value. If you genuinely need a mutable shared config, drop `freeze` and use `mut` (and accept the safety trade-off).

---

## 35. Missing required env var at build time

```toml
[env]
required = ["DATABASE_URL"]
```

```python
comptime let DB_URL: str = env("DATABASE_URL")
```

If `DATABASE_URL` is unset:

```
error[tyc::comptime]: required env var `DATABASE_URL` is unset
                      (declared in [env] required)
```

`tyc build` fails at compile time, not at runtime. This is feature, not bug — set the env var or remove it from `[env] required`.

---

## 36. Top-level filename matches a Python stdlib module (v0.6.0)

```
src/
└── types.ty            # ❌ tyc::stdlib_module_shadow
```

The emitted `build/types.py` would land on `sys.path` and intercept `import types` everywhere — transitive stdlib imports like `dataclasses → types` would resolve to your file. Severity `warn` by default; non-fatal but worth respecting.

**Fix:** rename to `lang_types.ty`, `records.ty`, etc. Only fires for files at the top of the configured source directory (v0.7.0); nested files like `src/indexer/tokenize.ty` are exempt because they lower to `build/indexer/tokenize.py` which is NOT on `sys.path`.

---

## 37. `pub *` in a non-`__init__.ty` module (v0.7.0)

```python
# src/mypkg/handlers.ty
pub *                # ❌ tyc::pub_star_outside_init (advice)
```

`pub *` only has meaning in `__init__.ty` — the desugar pass aggregates sibling modules' `pub` names there. Outside `__init__.ty` it's a no-op with confusing intent.

**Fix:** move to `__init__.ty`, or remove.

---

## 38. Two siblings exporting the same `pub` name (v0.7.0)

```
src/mypkg/
├── __init__.ty       # pub *
├── a.ty              # pub def hello() -> str: ...
└── b.ty              # pub def hello() -> str: ...
```

```
error[tyc::pub_name_collision]: `pub *` aggregation in __init__.ty would re-export
                                duplicate name `hello` from siblings `a` and `b`
```

**Fix:** rename one, drop the `pub` on one, or replace `pub *` with explicit re-exports:

```python
# __init__.ty
from .a import hello as a_hello
from .b import hello as b_hello

__all__ = ["a_hello", "b_hello"]
```

---

## 39. Reading a `let NAME: T` before it's been assigned on every path (v0.7.0)

```python
def parse(raw: str) -> int:
    let value: int                    # declare-only
    if raw.isdigit():
        value = int(raw)
    return value                       # ❌ tyc::use_of_uninitialised
```

```
error[tyc::use_of_uninitialised]: `value` may be read before assignment
                                  (not assigned on the `else` path)
```

Sibling `match` arms and sibling `if`/`elif`/`else` bodies each count as a separate first-assignment path. `return`/`raise`/`continue`/`break` arms are excluded from the intersection. Loops do NOT propagate body assignments (body may execute zero times).

**Fix:** initialise inline (`let value: int = 0`), assign in every non-diverging arm, or diverge on the missing path:

```python
def parse(raw: str) -> int:
    let value: int
    if raw.isdigit():
        value = int(raw)
    else:
        raise ValueError(f"not a digit: {raw}")
    return value                       # ✅
```

---

## 40. Non-default field declared after a defaulted one (v0.7.0)

```python
class Worker:
    name: str
    retries: int = 3
    queue_size: int           # ❌ tyc::field_default_ordering
```

The synthesised `__init__` follows declaration order; Python rejects a non-default param after a default one — would raise `TypeError` at import.

**Fix:** reorder, putting every non-defaulted field above defaulted ones:

```python
class Worker:
    name: str
    queue_size: int
    retries: int = 3
```

Or use a factory function for awkward defaults.

---

## 41. `let`-shadowing (NOT allowed — always)

```python
def demo() -> None:
    let x: int = 1
    if cond:
        let x: int = 2          # ❌ tyc::no_block_shadow
```

Python has no block scope. The inner `let x` would actually rebind the outer `x` at runtime, which conflicts with `let`'s immutability semantics.

**Fix:** use `mut` for the outer binding (and don't re-`let`) — or pick a different name. Sibling `case`/`if`/`elif` arms each get fresh-binding behaviour (v0.6.0+/v0.7.0+) so the diagnostic only fires on true shadow situations.

---

## 42. `lazy let X: T:` colon-block form

```python
lazy let CONFIG: Config:    # ❌ does NOT parse
    return load_from_disk()
```

The colon-block form was designed but is not implemented. Use the expression form:

```python
lazy let CONFIG: Config = load_from_disk()
```

---

## 43. `lazy[T]` return type

```python
def primes(n: int) -> lazy[list[int]]:     # parses but not yet implemented
    ...
```

Designed but unimplemented today. Use `Iterator[T]` directly:

```python
def primes(n: int) -> Iterator[int]:
    ...
    yield from (i for i in range(2, n + 1) if sieve[i])
```

---

## 44. `model X frozen:`

```python
model ApiUser frozen:           # ❌ does NOT parse
    id: int
```

`frozen` is a modifier on `class` only. For an immutable Pydantic model, configure Pydantic's `frozen=True` via `model_config = ConfigDict(frozen=True)`:

```python
model ApiUser:
    model_config = ConfigDict(frozen=True, extra="forbid")
    id: int
```

(Note: Pydantic's `frozen=True` is *faux* immutability — only blocks field reassignment, not nested mutation.)

---

## 45. Multi-line `|>` pipes without wrapping parens

```python
let result = x
    |> normalise()
    |> scale(2.0)
    |> clamp(0.0, 1.0)
```

Will not parse. **Wrap the whole chain in parens:**

```python
let result = (
    x
    |> normalise()
    |> scale(2.0)
    |> clamp(0.0, 1.0)
)
```

---

## 46. Nullary sealed-union variants matched as `Foo(_)`

```python
type State = Red | Green | Yellow
class Red: pass
class Green: pass
class Yellow: pass

match s:
    case Red(_):     ...    # ❌ never matches — `_` is a positional capture for a 0-field class
    case Green():    ...
    case Yellow():   ...
```

The Python `match` spec interprets `case Red(_):` as "match a class with exactly one positional field and bind it to `_`". Nullary variants have **no** positional fields, so this never matches.

**Fix:** `case Red():` (two empty parens). And declare nullary variants explicitly:

```python
class Red:    pass
class Green:  pass
class Yellow: pass
```

---

## 47. `from typing import List` / `Dict` etc.

```python
from typing import List, Dict, Tuple   # ❌ tyc::typing_alias_deprecated
```

Use lowercase built-ins:

```python
let xs: list[int] = [1, 2, 3]
let d:  dict[str, int] = {"a": 1}
let t:  tuple[int, str] = (1, "a")
```

---

## 48. Forgetting to gate VM execution

When working with stress harnesses or running uncheck'd Typhon snippets, you might want to skip the pre-VM `tyc check` (v0.3.1):

```bash
TYC_SKIP_CHECK=1 tyc run experimental.ty
```

Without this, `tyc run` (VM) refuses to execute Typhon that doesn't type-check. `--compile` mode has no equivalent bypass (the build pipeline always type-checks).

---

## 49. Two `impl`/`extend` blocks defining the same method (v0.3.1)

```python
impl Box:
    def get(self) -> int:
        return self.value

extend Box:
    def get(self) -> int:           # ❌ tyc::duplicate_method
        return self.value * 2
```

Multiple `impl`/`extend` blocks merge; duplicates would silently lose one. Also fires when the same method exists on both `impl Union:` (distributed via v0.6.0) and `impl Variant:`.

**Fix:** rename, delete the duplicate, or merge bodies.

---

## 50. `class P frozen: x: int = 0; y: int` — order of frozen-class fields

Same issue as #40 above, but worth calling out explicitly for `frozen` classes:

```python
class P frozen:
    x: int = 0
    y: int           # ❌ tyc::field_default_ordering
```

`frozen` doesn't change the ordering rule.

**Fix:**

```python
class P frozen:
    y: int
    x: int = 0
```

---

## 51. Calling a non-existent method on a class instance (v0.8.0)

```python
class Repo:
    items: list[str]

def demo(r: Repo) -> None:
    r.dleete()              # ❌ tyc::attribute_not_found
```

v0.8.0 widened `tyc::attribute_not_found` to fire on class-instance / generic-class receivers, not just `TypeVar`-bounded ones. v0.8.1 added the `partial` shape marker on `InterfaceShape` so foreign / venv-introspected classes (`uvicorn.Server`, `httpx.AsyncClient`, `fastapi.Request`) stay lenient.

**Fix:** correct the spelling, or wrap an intentional dynamic call in `unsafe:`.

---

## 52. Mismatched parameter type when implementing an interface (v0.8.0)

```python
interface Repo:
    def save(self, item: str) -> bool

class BadRepo:
    def save(self, item: int) -> bool:        # ❌ tyc::interface_not_conforming
        ...
```

v0.8.0 added param-type position-by-position contravariant checking to `interface_missing_members`. Arity matched before but param-type mismatches went silently.

**Fix:** align the impl's parameter types with the interface's (or generalise the interface).

---

## 53. Passing a non-singleton literal to a string-literal type (v0.8.0)

```python
type Color = "red" | "green" | "blue"

def paint(c: Color) -> None: ...
paint("orange")                                # ❌ tyc::type_mismatch
```

v0.8.0 added `Type::LitStr` — string-literal singleton types. `type Color = "red" | "green" | "blue"` and `Literal["a", "b"]` produce `LitStr` slots; assignability rejects non-matching literals. Bidirectional inference widens string literals to `LitStr` only when the expected type carries one, so unannotated `let s = "hi"` still infers plain `str`.

---

## 54. `?` in a `with`-chain routing the wrong error type (v0.8.0)

```python
def load() -> Result[Cfg, IoErr]:
    with raw = read_text("c.toml")?:           # parse_err is ParseErr, not IoErr — ❌
        return Ok(parse_cfg(raw)?)
```

v0.8.0 added `?` propagation inside `with`-chains. `result_error_mismatch` now fires when the implicit return form routes a mismatching error type through the chain.

**Fix:** normalise the inner error to the outer one via `.map_err(_to_io_err)?`.

---

## 55. Match capture shadowing an outer name (v0.8.0)

```python
let key = "incoming"
match e:
    case Click(key):                           # ❌ tyc::pattern_shadows_outer
        ...
```

The pattern `Click(key)` binds a *new* `key` that shadows the outer `let key = "incoming"`. Subtle bug: the pattern matches anything (rather than checking `e.key == "incoming"`).

**Fix:** rename the pattern binding (`Click(target_key)`) or use a guard if you wanted equality (`case Click(k) if k == key: ...`).

---

## 56. `newtype Foo = "literal"` (v0.8.0)

```python
newtype Bogus = "string literal"               # ❌ tyc::newtype_invalid_base
newtype X = 42                                 # ❌ tyc::newtype_invalid_base
```

A newtype base must be a type expression — literal RHS used to be silently accepted and resolve to `Unknown`. v0.8.0 rejects with a dedicated diagnostic.

**Fix:** use a proper type expression — `newtype UserId = int`, `newtype Color = str`.

---

## 57. Programs relying on the VM's silent integer overflow (v0.8.0)

```python
def big() -> int:
    return 2 ** 100        # Now returns the actual 31-digit number, not a wrapped i64
```

v0.8.0 switched the VM's `Value::Int` to `num_bigint::BigInt`. Programs that **relied** on the previous silent i64 wrap-around (rare, but a few cryptographic / hashing exercises did) will now compute different (correct) results.

**Fix:** if you actually want modular arithmetic, do it explicitly — `(2 ** 100) & 0xFFFFFFFFFFFFFFFF`.

---

## 58. `let xs = []` — empty collection without annotation (v0.8.0)

```python
let xs = []        # ⚠️ tyc::empty_collection_no_annotation
```

v0.8.0 added a lint warning when an empty `[]` / `{}` / `set()` is written without an annotation or expected type. The element type can't be inferred and downstream operations will all see `list[Unknown]`.

**Fix:** annotate or seed with one element of the target type:

```python
let xs: list[str] = []
mut xs = [first_value]
```

---

## 59. Bare `List[…]` / `Optional[…]` / `Dict[…]` / `Union[…]` in annotations (v0.8.0)

```python
def f(items: List[str]) -> None: ...           # ⚠️ tyc::typing_alias_in_annotation
def g(x: Optional[int]) -> None: ...           # ⚠️ tyc::typing_alias_in_annotation
```

v0.8.0 added a lint warning matching the existing import-level `typing_alias_deprecated`. Use lowercase built-ins / Typhon sugar instead:

```python
def f(items: list[str]) -> None: ...
def g(x: int?) -> None: ...
```

---

## 60. Hard-coded secret in a `*_TOKEN` / `*_SECRET` / `*_KEY` binding (v0.8.0)

```python
let API_KEY = "sk-live-abc123def456"           # ⚠️ tyc::contains_secret_literal
```

v0.8.0 added a heuristic lint for inline string literals named `*_(TOKEN|SECRET|PASSWORD|PWD|KEY|API_KEY)`. The warning is suppressed when the project's `[strictness] allow-secret-comptime = true`, or when the binding is comptime-loaded from `env(...)`.

**Fix:** read from the environment via `comptime let API_KEY = env("API_KEY")`.

---

## 61. Calling a `Result` combinator under `tyc run` (v0.9.0)

```python
def parse(s: str) -> Result[int, str]: ...

def doubled(s: str) -> Result[int, str]:
    return parse(s).map(lambda n: n * 2)   # ✓ since v0.9.0 — was AttributeError before
```

Before v0.9.0 the VM did not bind the combinator methods on `Ok` / `Err`, so a typecheck-clean program crashed at run-time with `AttributeError: Ok has no attribute 'map'`. v0.9.0 binds `.map` / `.map_err` / `.and_then` / `.or_else` as native methods.

**Fix:** no source change needed — rebuild against tyc ≥ v0.9.0.

---

## 62. `open(path, "w")` under `tyc run` (v0.9.0)

```python
def save(path: str, body: str) -> None:
    with open(path, "w") as f:
        f.write(body)                    # ✓ since v0.9.0 — was unsupported mode
```

Before v0.9.0 the VM's `open()` only handled read mode. v0.9.0 honours `"r"` / `"w"` / `"a"` / `"wb"` / `"r+"` and friends, plus the `with`-block protocol via `__enter__` / `__exit__`. `json.load` / `json.dump` ride on top.

---

## 63. `case str() as s:` under `tyc run` (v0.9.0)

```python
def label(x: object) -> str:
    match x:
        case str() as s: return f"str:{s}"
        case int() as n: return f"int:{n}"
        case _:          return "other"
```

Before v0.9.0 the VM's `match` only supported nominal patterns on user-defined classes. v0.9.0 matches `case str() as s:` / `case int() as n:` / `case float()` / `case list()` / `case dict()` / `case bool()` / `case bytes()`. The exhaustiveness checker also recognises `case None:` + `case str() as s:` as covering `str?`.

---

## 64. `frozenset` as a dict key under `tyc run` (v0.9.0)

```python
mut groups: dict[frozenset[str], int] = {}
let key: frozenset[str] = frozenset({"a", "b"})
groups[key] = 1                          # ✓ since v0.9.0 — was TypeError before
```

Before v0.9.0 the VM treated `frozenset` as unhashable. v0.9.0 adds a `HashKey::FrozenSet` variant with insertion-order-independent hashing.

---

## 65. `freeze let CFG = {...}` actually freezing under `tyc run` (v0.9.0)

```python
freeze let CFG: dict[str, int] = {"port": 8080}

def boot() -> None:
    CFG["port"] = 9999                   # ❌ TypeError under both tyc run and tyc build
```

Before v0.9.0 the VM treated `freeze let` as a regular `let`, so mutations through aliased references silently succeeded. v0.9.0 recursively wraps the value (list → tuple, dict → mappingproxy-tagged dict, set → frozenset) so the VM and `tyc build && python` behave identically.

Plus a check-time guard: since v0.9.0 the type checker pre-validates the RHS via `tyc::freeze_not_freezable`. Constructing a non-`frozen` user class on the RHS is rejected at `tyc check` time instead of failing at first import.

---

## 66. `comptime let X = env(...)` under `tyc run` (v0.9.0)

```python
comptime let PORT = int(env("PORT", "8080"))   # ✓ since v0.9.0
```

Before v0.9.0 the VM didn't run the substitution pass, so `comptime let PORT = ...` crashed under `tyc run` with `NameError: env is not defined`. v0.9.0 shares the substitution pass with `tyc build`, so `tyc run`, `tyc check`, and the compiled output all see the same inlined constants.

---

## 67. `class!` exception fields surviving `except X as e:` (v0.9.0)

```python
class! HttpError(Exception):
    code: int
    message: str

def main() -> None:
    try:
        raise HttpError(code=404, message="not found")
    except HttpError as e:
        print(e.code, e.message)         # ✓ since v0.9.0
```

Before v0.9.0 the VM's `class!` declarations didn't run the synthesised `__init__`, so user fields weren't initialised and `e.code` raised `AttributeError`. v0.9.0 wires the synthesised `__init__` through, and exception-type matching walks the MRO.

---

## 68. `*args` / `**kwargs` without annotations (v0.9.0)

```python
def trace(f, *args, **kwargs):           # ❌ tyc::missing_annotation
    ...
```

v0.9.0 enforces Rule 1 (every parameter annotated) on `*args` / `**kwargs` too. Canonical idiom is `object`:

```python
def trace[R](f: Callable[..., R], *args: object, **kwargs: object) -> R:
    return f(*args, **kwargs)
```

For typed variadics use a `TypeVarTuple` / a fixed-arity overload pattern depending on what you mean.

---

## 69. `assert x is not None` not narrowing (v0.9.0)

```python
def f(x: str?) -> int:
    assert x is not None
    return len(x)                        # ✓ since v0.9.0 — x narrowed to str
```

Before v0.9.0 the type checker treated `assert` as a no-op for narrowing purposes; Pyright / mypy / pyrefly all narrow it. v0.9.0 brings parity.

---

## 70. `while True:` loops marking the post-loop as unreachable (v0.9.0)

```python
def serve() -> Never:
    while True:
        handle_request()                  # body never breaks
    # post-loop point is unreachable since v0.9.0
```

Before v0.9.0 the type checker required an unreachable `return` after `while True:` with no `break`. v0.9.0 recognises the body's "always returns/raises with no break" pattern and treats the post-loop point as unreachable, so `missing_return` doesn't fire and there's nothing to write.

Plus post-while-loop narrowing: after `while y is None: y = load()` (no `break`), `y` is narrowed to non-`None` after the loop.

---

## 71. `Cons[T]` not assignable to `LinkedList[T]` (v0.9.0)

```python
type LL[T] = Cons[T] | Nil
class Cons[T] frozen:
    head: T
    tail: LL[T]
class Nil frozen: pass

def length[T](xs: LL[T]) -> int:
    mut cur: LL[T] = xs
    while True:
        match cur:
            case Cons(head, tail):
                cur = tail                # ✓ since v0.9.0 — Cons[T] assignable to LL[T]
            case Nil():
                return 0
```

Before v0.9.0 variant-to-parametric-union flow didn't deduce `Cons[T] → LL[T]`, so the loop body raised `tyc::type_mismatch`. v0.9.0 covers it.

---

## 72. `extend list:` not dispatching on `list[T]` receivers (v0.9.0)

```python
extend list:
    def first_or[T](self, default: T) -> T:
        return self[0] if self else default

def head(xs: list[int]) -> int:
    return xs.first_or(0)                # ✓ since v0.9.0 — was attribute_not_found before
```

Before v0.9.0 the dispatch table only consulted user-class shapes; a `list[T]`-annotated receiver fell through to `attribute_not_found`. v0.9.0 consults the synthetic `__typhon_builtin_ext_list` class shape first.

---

## 73. `func[T](args)` explicit type instantiation (v0.9.0)

```python
def identity[T](x: T) -> T: return x

def main() -> None:
    let y: int = identity[int](7)        # ❌ tyc::operator_type_mismatch (since v0.9.0)
```

Before v0.9.0 this crashed at runtime with `TypeError: 'function' object is not subscriptable`. v0.9.0 fires a clear check-time error pointing users at the bidirectional inference pattern (drop the `[int]` — `T` is inferred from the argument).

---

## 74. `comptime let T: type = int` (v0.9.0)

```python
comptime let Id: type = int
def f(x: Id) -> Id: return x             # ✓ since v0.9.0
```

v0.9.0 lowers `comptime let T: type = <Type>` to a PEP 695 `type T = <Type>` alias statement so `T` is substitutable wherever a type is expected. `tyc check` runs the substitution before parsing the resolved module so check, build, and VM all see the same shape.

---

## 75. Multi-file projects under `tyc run` (v0.9.0)

```
src/main.ty:        from .repo.users import load
src/repo/__init__.ty
src/repo/users.ty:  def load() -> User: ...
```

Before v0.9.0 the VM only loaded `main.ty` and crashed on the first `from .X import` with `ImportError`. v0.9.0 walks the project source root, honours relative imports, and caches each module's bindings as a `Value::Module`. `tyc run --compile` now spawns `python -m <pkg>.main` so relative imports in the entry point resolve correctly.

---

## 76. Hand-rolling an enum instead of `enum Name:` (v0.11.0)

```python
# Works, but verbose — the auto-skip path keeps it as-is:
class Color(enum.Enum):
    RED = 1
    GREEN = 2

# Idiomatic since v0.11.0:
enum Color:
    RED                          # auto-numbered via enum.auto()
    GREEN
```

**Trigger:** reaching for `class X(enum.Enum):` (or `class!`) for a fixed set of named members. **Fix:** use `enum Name:` — bare members auto-number with `enum.auto()`, explicit `MEMBER = value` is preserved, `import enum` is injected for you, and `tyc fmt` round-trips it. The form parses, type-checks, **and** runs under `tyc run` (the VM has a native `enum` shim as of v0.11.0). `frozen` does not apply to `enum`; methods still go in an `impl Color:` block.

## 77. Expecting an infinite or lazily-consumed generator under `tyc run` (v0.10.0)

```python
def naturals() -> Iterator[int]:
    mut n: int = 0
    while True:
        yield n                  # ❌ under tyc run: collected eagerly → RuntimeError at 1M
        n = n + 1
```

**Trigger:** a `yield` function that never terminates, or one you only want to pull a few values from. **Diagnostic:** `RuntimeError` once `GENERATOR_CAP = 1_000_000` is hit. **Why:** the tree-walking VM can't suspend a frame (`Rc` values aren't `Send`), so generators run to completion and return an iterator over the *collected* values. **Fix:** finite generators are fine under `tyc run`; for genuinely infinite / lazy ones use `tyc build && python build/main.py`, which emits a real Python generator.

## 78. `@contextmanager` generator driven by a `with` under `tyc run` (v0.10.0)

```python
@contextmanager
def timer() -> Iterator[None]:
    let start: float = time.perf_counter()
    yield                        # ❌ under tyc run: setup+teardown both run at call time
    print(time.perf_counter() - start)
```

**Trigger:** a generator-based context manager used in a `with` block. **Why:** eager generator collection runs the setup *and* teardown at call time, so the `with` body can't execute between them. The decorator is recognised and `@contextmanager` *factory bodies* are exempt from `resource_not_managed`, but the driven-by-`with` case needs the real Python coroutine. **Fix:** `tyc build` for these; class-based `__enter__` / `__exit__` and `open()` work under the VM.

## 79. A wrong-*typed* argument to a third-party call now fails `tyc check` (v0.12.0)

```python
import sdk                        # a dependency that ships INLINE type hints (PEP 561 + annotations)
let r = sdk.fetch(12345)         # ❌ tyc::type_mismatch: expected str, found int (v0.12.0)
                                 #    sdk.fetch is `def fetch(url: str, ...) -> Response`
```

**Trigger:** passing the wrong type to a fully-typed pure-Python dependency *that ships inline annotations*. Before v0.12.0 only *arity* was checked, so this passed `tyc check` and failed at runtime. Now venv signature introspection (`tyc-venv`) recovers the parameter *types* via `inspect.signature` and checks them — for **functions and constructors**. **Note:** the annotations must be *inline* in the package's own source. A stub-only library like `requests` (typed via typeshed's `types-requests`, not in its source) degrades to `Unknown` under this layer — catch those with `[checker] external = "ty"` (typeshed) or a `.dty` stub. **Fix:** pass the right type. **Corollary:** if the dependency *can't* be introspected at all (no `.venv`, not installed, C-extension-only), you'll see the `unintrospectable-dependency` warning instead of silent skipping — clear it with `uv sync`, a `.dty` stub, or `[checker] external = "ty"`. Tune severity via `[strictness] unintrospectable-dependency`.

## 80. Relying on the VM's old (identity-based) value semantics (v0.11.0 behaviour change)

```python
class Point:
    x: int
    y: int

print(Point(1, 2) == Point(1, 2))   # tyc run: False before v0.11.0, True now
```

**Trigger:** code whose output depended on the VM's pre-v0.11.0 behaviour. **What changed:** dataclass equality is now value-based (keyed on class *identity*, so same-named classes from different modules don't collide), instance `repr` is `Name(field=value, …)`, instances are hashable, set / frozenset equality is order-independent, and float `repr` is CPython's shortest round-tripping form. These now **match `tyc build && python`** — the old VM behaviour was the bug. **Fix:** none needed; just be aware `tyc run` output may differ from a pre-v0.11.0 run (it now agrees with CPython). Same spirit as the v0.8.0 BigInt switch.

## 81. `as!` nested in a call argument or spanning multiple lines (v0.14.0)

```python
let ok = validate(payload as! dict[str, int])      # ❌ v1: as! not supported in call args
let cfg = load(                                    # ❌ v1: as! must be on one physical line
    raw
) as! dict[str, int]
```

**Trigger:** putting `as!` anywhere other than the top of a single-line value position. **Why:** v1 lowers `as!` line-based in `tyc-syntax` (like the other sugar), and a cast casts the *entire* preceding value on one physical line — a nested or multi-line `as!` isn't recognised, so the `as!` survives to the parser and you get a clean `tyc::parse` error (never a silent miscompile). **Fix:** bind first, then cast on its own line:

```python
let raw_data = load(raw)                # multi-line / nested expression
let data = raw_data as! dict[str, int]  # then the cast, one line, top-level
ok = validate(data)
```

Supported positions: `let`/`mut` RHS, `x = …`, augmented `x += …`, `return …`, `yield …`, and bare expression statements. (The nested / multi-line forms are slated for the AST-based lowering migration, which lifts the same restriction off multi-line `?`.)

## 82. Expecting `as!` to enforce its check under `tyc run` (v0.14.0)

```python
let n = some_value as! int    # tyc run: passes even if some_value is a str
```

**Trigger:** relying on the boundary check to fire in the VM. **Why:** the in-process VM treats `as!` as an identity passthrough — the *recursive structural* runtime check lives in `typhon_runtime/cast.py` and runs on the compiled path. **Fix:** the static type is still pinned to the target everywhere; for the *runtime* enforcement, use `tyc build && python build/main.py` (or `tyc run --compile`). `tyc check` already rejects a target that doesn't satisfy the surrounding annotation, so most mistakes are caught before runtime regardless.

---

## Quick "which guide answers this" map

| If you're stuck on… | Read |
|---|---|
| `let`/`mut`, `T?`, narrowing | `docs/guides/02-values-and-types.md` |
| Function rules, `Callable`, generics syntax | `docs/guides/03-functions.md` |
| `guard`, comprehensions, collections | `docs/guides/04-control-flow-and-collections.md` |
| `class` / `model` / `impl` / `extend` / `frozen` | `docs/guides/05-classes-and-models.md` |
| `Result`, `?`, `with`-chains | `docs/guides/06-error-handling.md` |
| Sealed unions, exhaustive match | `docs/guides/07-sealed-unions-and-match.md` |
| Generics, interfaces, structural typing | `docs/guides/08-generics-and-interfaces.md` |
| `async`, `gather:`, `go`, free-threaded | `docs/guides/09-async-and-concurrency.md` |
| Pipes, `comptime`, `lazy`, `@pure`/`@memo`, `unsafe`, `.dty` | `docs/guides/10-advanced-features.md` |
| The "why" of any design call | `docs/long-term-plan.md` |
| Compiler internals / crate layout | `docs/architecture.md` |
| In-process VM | `docs/vm.md` and [RUNTIME.md](RUNTIME.md) |
| `typhon.toml` keys | `docs/configuration.md` |
| `tyc` flags | [CLI.md](CLI.md) and `docs/cli.md` |
| Multi-file projects | [PACKAGING.md](PACKAGING.md) |
| Canonical patterns | [COOKBOOK.md](COOKBOOK.md) |
| Diagnostic catalog | [DIAGNOSTICS.md](DIAGNOSTICS.md) and `tyc explain <code>` |
