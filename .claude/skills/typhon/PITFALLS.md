# Typhon — Extended Pitfalls Catalogue

The errors and surprises that bite people who try to write Typhon as if it were Python. Ranked roughly by frequency, with the diagnostic you'll see and the fix.

For each entry: **trigger → diagnostic → fix**.

Current release: **v0.7.0**. Pitfalls tagged with a version annotation landed in that release; older pitfalls predate v0.3.0.

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
