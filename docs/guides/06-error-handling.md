# 6. Error handling with `Result`

Exceptions exist in Python, and Typhon doesn't remove them. But for *expected* failures — parse errors, missing records, validation rejections — Typhon uses a typed `Result[T, E]`. The compiler tracks errors as values, the `?` operator propagates them, and `with`-chains sequence them like Elixir.

## The problem with exceptions

Python's exceptions don't appear in type signatures. A function annotated `def find_user(id: int) -> User` might raise `ValueError`, `KeyError`, `DatabaseError`, or anything else — and callers can't tell from the signature, the editor, or the type checker. The result is defensive `try/except`, missed cases, and stack traces in production.

`Result[T, E]` is the alternative: a return type that *carries* the failure as a value the caller must handle.

## `Result`, `Ok`, `Err`

`Result[T, E]` is a sealed sum type with two constructors:

- `Ok(value: T)` — success
- `Err(error: E)` — failure

```python
def parse_port(raw: str) -> Result[int, str]:
    if not raw.isdigit():
        return Err(f"not a number: {raw}")
    let n: int = int(raw)
    if n < 1 or n > 65535:
        return Err(f"port out of range: {n}")
    return Ok(n)
```

The return type — `Result[int, str]` — tells you everything the function can do: succeed with an `int`, or fail with a `str` describing why. The compiler enforces that both branches return a `Result`.

### Calling a `Result`-returning function

You have to handle both cases. Pattern-matching is the most explicit form:

```python
def main() -> None:
    match parse_port("8080"):
        case Ok(port):
            print(f"binding {port}")
        case Err(msg):
            print(f"bad port: {msg}")
```

The checker enforces that `match` on a `Result` covers both `Ok` and `Err`. (Sealed unions and exhaustive matching are guide 7.)

## The `?` operator

Manual `match` everywhere is noisy. The `?` suffix is sugar for "unwrap the `Ok` or short-circuit the `Err` to the enclosing function":

```python
def parse_address(host: str, port_str: str) -> Result[tuple[str, int], str]:
    let port: int = parse_port(port_str)?
    return Ok((host, port))
```

If `parse_port` returns `Err(msg)`, the `?` short-circuits and `parse_address` returns `Err(msg)` immediately. If it returns `Ok(n)`, `port` is bound to `n` and execution continues.

**The compiler checks that `?` is only used inside a function whose return type is a compatible `Result`.** Otherwise where would the early return go?

```python
def bad() -> int:
    let n: int = parse_port("8080")?    # ❌ tyc::invalid_question_op
    return n
```

```
error[tyc::invalid_question_op]: `?` can only short-circuit inside a function
                                              returning a compatible `Result`
 ┌─ src/main.ty:2:13
 │
2 │     let n: int = parse_port("8080")?
 │                  ^^^^^^^^^^^^^^^^^^^^ enclosing function returns `int`, not `Result`
```

The "compatible" check is structural: the error types must match (or unify, for generics). You can't `?` an `Err[ParseError]` out of a function returning `Result[T, IoError]` — convert it first.

### How `?` desugars

`?` does **not** lower to `try/except`. It's a plain inline check, keeping stack traces clean and predictable:

```python
# Typhon
let port: int = parse_port(port_str)?

# Emitted Python
_tmp_0 = parse_port(port_str)
if isinstance(_tmp_0, Err):
    return _tmp_0
port: int = _tmp_0.value
```

No hidden exception flow. Every short-circuit is visible in the emitted source.

## `with`-chains

Sequencing several `Result`-returning calls is common, and `?` plus assignment gets repetitive. Typhon borrows Elixir's `with` for this:

```python
def make_report(user_id: int) -> Result[Report, AppError]:
    with user   = db.find_user(user_id)?,
         perms  = check_perms(user)?,
         report = build_report(user, perms)?:
        return Ok(report)
    else err:
        log.warn(err)
        return Err(err)
```

Read it as: bind each name in turn, unwrapping `Ok` and binding the success value. If any step yields `Err`, jump to the `else err:` block with that error in scope.

The `else err:` block is optional. Without it, the first `Err` short-circuits straight out of the enclosing function (which must, of course, return a compatible `Result`).

### When to reach for `with`-chains

- Three or more `Result`-returning calls where each depends on the previous.
- You want a single failure-handling site (logging, metrics, rollback) for any step that fails.

For one or two calls, `?` on each line reads fine.

## Combinators on `Ok` and `Err`

`Ok` and `Err` carry four combinator methods on the runtime classes. They land in v0.6.0 for the compiled path and v0.9.0 for the in-process VM:

```python
def parse_and_double(raw: str) -> Result[int, str]:
    return parse_port(raw).map(lambda n: n * 2)

def parse_to_app(raw: str) -> Result[int, AppError]:
    return parse_port(raw).map_err(lambda msg: AppError(message=msg))

def step(raw: str) -> Result[Loaded, AppError]:
    return (
        parse_port(raw)
            .map_err(lambda msg: AppError(message=msg))
            .and_then(load_from_port)
    )
```

| Method | On `Ok(v)` | On `Err(e)` |
|---|---|---|
| `.map(f)` | `Ok(f(v))` | `Err(e)` |
| `.map_err(g)` | `Ok(v)` | `Err(g(e))` |
| `.and_then(f)` | `f(v)` (must return `Result`) | `Err(e)` |
| `.or_else(h)` | `Ok(v)` | `h(e)` (must return `Result`) |

`with`-chains + `?` and combinator chains are functionally equivalent — choose by readability. `with`-chains tend to read better when each step uses the previous step's value by name; combinators read better when each step is a one-argument function.

Before v0.9.0 the combinators worked under `tyc build && python build/main.py` but crashed under `tyc run` with `AttributeError: Ok has no attribute 'and_then'`. v0.9.0 binds them as native VM methods, so the same source runs identically under both modes.

## `?` inside a comprehension is rejected

Comprehensions lower to nested `for`-loops in Python, so the surrounding function frame `?` would short-circuit out of is *not* the comprehension's frame:

```python
def collect(xs: list[str]) -> Result[list[int], str]:
    return Ok([parse(x)? for x in xs])   # ❌ tyc::invalid_question_op
```

Pre-extract with an explicit loop, or chain `.and_then` / `.map`:

```python
def collect(xs: list[str]) -> Result[list[int], str]:
    mut out: list[int] = []
    for x in xs:
        out.append(parse(x)?)
    return Ok(out)
```

Since v0.9.0 the `tyc::invalid_question_op` help text explicitly mentions this carve-out (the previous text only covered the Result-return cause).

## Choosing your error type

`E` in `Result[T, E]` can be anything. Three patterns by complexity:

### 1. A plain string (good for prototypes)

```python
def parse_port(raw: str) -> Result[int, str]: ...
```

Easy to read, easy to write, but you can't pattern-match on the *kind* of failure — only the text.

### 2. A sealed union (good for domain code)

```python
type ParseError = NotANumber | OutOfRange

class NotANumber:
    raw: str

class OutOfRange:
    value: int
    min: int
    max: int

def parse_port(raw: str) -> Result[int, ParseError]: ...
```

Now callers can `match` on the specific failure and react. (See [guide 7](07-sealed-unions-and-match.md) for the full story.)

### 3. A class hierarchy (good for cross-cutting errors)

```python
class AppError:
    message: str
    correlation_id: str
```

Use this when you have many call sites that just need a uniform shape to log/return.

## Bridging exceptions and `Result`

Third-party Python libraries raise exceptions. Wrap them at the boundary:

```python
import json

def load_config(path: str) -> Result[dict[str, str], str]:
    try:
        with open(path) as f:
            return Ok(json.load(f))
    except FileNotFoundError:
        return Err(f"config not found: {path}")
    except json.JSONDecodeError as e:
        return Err(f"invalid JSON: {e}")
```

After this wrapper, downstream code can use `?` and `with`-chains without touching `try`.

### `try_result` and postfix `rescue` — bridge without writing `try`

When you map *distinct* exception types to *distinct* errors, the explicit `try/except` shim above is the clearest form. But for the common single-boundary case — "run this; if it throws, turn the exception into my error" — you don't need `try`/`except` at all.

The `try_result(thunk[, on_err])` combinator runs `thunk()` and returns `Ok(result)`, or `Err(on_err(exc))` on any exception (or `Err(exc)` when no mapper is given):

```python
def load_config(path: str) -> Result[dict[str, str], str]:
    return try_result(lambda: json.load(open(path)), lambda e: f"bad config: {e}")
```

The lambdas are the ugly part. The postfix **`rescue`** operator says the same thing with none:

```python
def load_port(raw: str) -> Result[int, str]:
    let n: int = int(raw) rescue e: f"bad port: {e}"     # catch + map + propagate
    return Ok(n)
```

`EXPR rescue NAME: ERR_EXPR` runs `EXPR`; on any exception it binds the exception to `NAME`, evaluates `ERR_EXPR`, and propagates `Err(ERR_EXPR)` to the enclosing function — exactly like `?`, so the function must return a compatible `Result`. It's surface sugar for `try_result(lambda: EXPR, lambda NAME: ERR_EXPR)?` and lowers to the same emitted code. One fallible step per line, mapped to one error, reads straight down:

```python
def load_config(text: str) -> Result[Config, str]:
    let data: dict[str, str] = json.loads(text) as! dict[str, str] rescue e: f"bad json: {e}"
    let port: int           = int(data["port"])                    rescue e: f"bad port: {e}"
    return Ok(Config(host=data["host"], port=port))
```

`rescue` works in value positions and after a leading `return` / `if` / `while` / `assert`, and composes with `as!` as shown.

When several statements share one catch-all mapping, the **block form** avoids repeating `rescue e: …` on every line:

```python
def load_config(text: str) -> Result[Config, str]:
    rescue e: f"bad config: {e}":
        let data: dict[str, str] = json.loads(text) as! dict[str, str]
        return Ok(Config(host=data["host"], port=int(data["port"])))
```

Any exception raised in the suite is mapped through the error expression and returned as `Err(...)` — the lambda-free replacement for a `try/except` shim. The mapped error type is checked against the function's declared error type in both forms. Reach for the explicit multi-`except` `try` shim only when you need to map several exception *types* to different errors.

## What gets emitted

`Result`, `Ok`, and `Err` are emitted as tagged dataclasses in a generated `typhon_runtime.py` module that sits in your output tree:

```python
# build/typhon_runtime.py (excerpt)
from dataclasses import dataclass
from typing import Generic, TypeVar

T = TypeVar("T")
E = TypeVar("E")

@dataclass(slots=True, frozen=True)
class Ok(Generic[T]):
    value: T

@dataclass(slots=True, frozen=True)
class Err(Generic[E]):
    error: E

Result = Ok[T] | Err[E]    # roughly
```

The point: there is no PyPI dependency. Production servers run the emitted Python plus this small runtime helper, which is generated alongside your code.

## Putting it together

A small worked example: read a config file, parse a port number, and report errors clearly.

```python
import json

type ConfigError = NotFound | InvalidJson | MissingKey | BadPort

class NotFound:
    path: str

class InvalidJson:
    detail: str

class MissingKey:
    key: str

class BadPort:
    raw: str

def load(path: str) -> Result[dict[str, str], ConfigError]:
    try:
        with open(path) as f:
            return Ok(json.load(f))
    except FileNotFoundError:
        return Err(NotFound(path=path))
    except json.JSONDecodeError as e:
        return Err(InvalidJson(detail=str(e)))

def get_port(cfg: dict[str, str]) -> Result[int, ConfigError]:
    let raw: str? = cfg.get("port")
    guard r = raw else:
        return Err(MissingKey(key="port"))
    if not r.isdigit():
        return Err(BadPort(raw=r))
    return Ok(int(r))

def boot(path: str) -> Result[int, ConfigError]:
    with cfg  = load(path)?,
         port = get_port(cfg)?:
        return Ok(port)

def main() -> None:
    match boot("config.json"):
        case Ok(port):
            print(f"listening on {port}")
        case Err(NotFound(path)):
            print(f"missing: {path}")
        case Err(InvalidJson(detail)):
            print(f"corrupt: {detail}")
        case Err(MissingKey(key)):
            print(f"config missing `{key}`")
        case Err(BadPort(raw)):
            print(f"`{raw}` is not a valid port")
```

The whole error-handling story shows up here:

- Errors are values, with a sealed-union type (`ConfigError`).
- `?` short-circuits inside `boot`, which composes two `Result`-returning calls.
- The `match` in `main` is exhaustive over every error variant — and the checker enforces that. Add a new variant to `ConfigError` and the match goes red until you handle it.

## Common mistakes

**Using `?` in a non-`Result` function:**

```python
def main() -> None:
    let port: int = parse_port("8080")?    # ❌ main returns None
```

Fix: change the signature, or `match` explicitly.

**Mismatched error types:**

```python
def parse_port(raw: str) -> Result[int, str]: ...

def boot(raw: str) -> Result[int, ConfigError]:
    let port: int = parse_port(raw)?       # ❌ str is not assignable to ConfigError
    return Ok(port)
```

Fix: convert the error at the boundary, either with a helper or by using `match`:

```python
def boot(raw: str) -> Result[int, ConfigError]:
    match parse_port(raw):
        case Ok(port):
            return Ok(port)
        case Err(msg):
            return Err(BadPort(raw=raw))
```

**Forgetting one variant in `match`:**

The checker counts variants and refuses to compile with `error[tyc::non_exhaustive_match]` listing the ones you missed. Add them or use `case _:` for a deliberate catch-all.

## What you've learned

- `Result[T, E]` makes failure visible in signatures; `Ok` and `Err` are its two constructors.
- `?` propagates errors with one character; the checker enforces compatible return types.
- `with`-chains sequence `Result`-producing calls with a single failure-handling site.
- Errors emit as small dataclasses in a generated `typhon_runtime.py` — no PyPI runtime.
- Wrap exception-raising library calls in a `try` shim and lift them into `Result`.

Next: [Sealed unions and `match`](07-sealed-unions-and-match.md) — declaring sum types and matching on them exhaustively.
