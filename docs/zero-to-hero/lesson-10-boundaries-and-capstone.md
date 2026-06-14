# Lesson 10 — Boundaries, power tools, and a capstone

*Zero to Hero · Lesson 10 of 10*

The final lesson: how to cross into untyped code safely (Rule 5), a grab-bag of power tools, the workflow you'll live in, and a capstone program that ties the whole series together.

## The untyped boundary

`Any` doesn't leak in by accident. When you call an untyped library or parse a raw payload, you cross a boundary *deliberately*. Three tools, in increasing permanence:

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

For a library you call all over, write a `.dty` stub once — strictly-typed Typhon describing its API. The compiler also ships bundled stubs for popular libraries (`httpx`, `requests`) and introspects installed packages' signatures automatically, so a wrong-typed argument to a third-party function is often caught with zero authoring.

## Power tools

**`comptime`** evaluates at *build time* and inlines the result as a literal — config-from-env with no runtime cost:

```python
comptime let PORT: int = int(env("PORT", "8080"))
comptime let IS_PROD: bool = env("BUILD_TAG", "dev") == "prod"
```

The sandbox is hermetic (no I/O, no loops, no imports) — pass everything in as arguments. Don't put secrets here; they'd be baked into the artifact (`tyc::contains_secret_literal` warns you).

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

**`guard`** is early-return narrowing (you met it in Lesson 2):

```python
def first_name(user: User?) -> str:
    guard u = user else: return "anonymous"
    return u.name
```

Also in the box: `@pure` (assert a function is side-effect-free) and `@memo` (`@functools.cache`).

## The `tyc` workflow

Your daily commands:

| Command | What it does | When |
|---|---|---|
| `tyc check src/` | parse + type-check, no output | constantly — your fast loop and the CI gate |
| `tyc run` | execute in the in-process VM | iterating on pure-Typhon logic |
| `tyc build` | full pipeline → `build/*.py` | producing shippable Python |
| `tyc fmt src/` | format (whitespace + `ruff format`) | pre-commit |
| `tyc explain <code>` | explain a `tyc::` diagnostic offline | when an error needs context |
| `tyc cheatsheet` | the 30-second syntax table | memory jog |
| `tyc migrate app.py` | typed Python → Typhon | porting an existing file |

If your program imports CPython-only libraries, `tyc run` falls back via `tyc run --compile` (build then exec).

The diagnostics you'll meet first:

| Code | Meaning | Fix |
|---|---|---|
| `tyc::missing_annotation` | unannotated param or return | add the type (`-> None` if nothing) |
| `tyc::missing_binding_kind` | bare `=` local | add `let` or `mut` |
| `tyc::immutable_assign` | reassigned a `let` | use `mut` |
| `tyc::nullable_use` | used a `T?` where `T` required | narrow it first |
| `tyc::non_exhaustive_match` | a sealed-union case is missing | add the `case` |
| `tyc::manual_init` | wrote `__init__` | delete it — it's generated |
| `tyc::method_in_class_body` | method in `class` not `impl` | move it to an `impl` block |
| `tyc::invalid_question_op` | `?` outside a `Result` function | fix the signature or `match` |

The golden rule: **when in doubt, run `tyc check`.** The compiler is the source of truth.

## Capstone

Let's combine the series — `newtype` IDs, a `model` boundary, `as!`, `Result`/`?`, a sealed union, and exhaustive `match` — into one program. Save as `src/main.ty` and run `tyc run`.

```python
import json

newtype UserId = int


# A validated boundary type — parsing untrusted input goes through `model`.
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

What each rule bought you:

- **`newtype UserId`** means a raw `int` can't reach a user-id slot — the `UserId(next_id)` wrap is the only way in.
- **`model SignupRequest`** validates the shape at the boundary; **`as!`** checks each untyped JSON field at runtime.
- **`Result` + `?`** make the "bad input" path explicit and impossible to forget — a malformed line becomes an `Err`, not a crash.
- **`type Outcome = Accepted | Rejected`** plus the `match` in `render` means adding a third outcome later forces you to handle it everywhere.

That's the Typhon value proposition in ~50 lines: the type system did the bookkeeping so the bugs never compiled.

## Try it

1. Add a third outcome, `Flagged` (with a `reason: str`), to `Outcome`. Run `tyc check` and let it guide you to every `match` that needs the new case.
2. Make `decide` flag anyone whose `name` is empty.
3. Add a `model`-validated `email: str?` field and surface it in the `Accepted` message.

## Where to go next

You now know enough to write real Typhon. To go deeper:

- **The numbered programming guides** ([`docs/guides/`](../guides/README.md)) — one focused chapter per feature, each with the emitted Python and diagnostics.
- **The example corpus** ([`examples/`](../../examples/)) — runnable exercises plus 15 production-shaped multi-file apps under [`examples/apps/`](../../examples/apps/).
- **The language reference** ([`docs/language.md`](../language.md)) — the canonical spec.
- **The cheatsheet** ([`docs/cheatsheet.md`](../cheatsheet.md) or `tyc cheatsheet`) and the **CLI reference** ([`docs/cli.md`](../cli.md)).
- **`tyc explain <code>`** — your in-terminal tutor for any diagnostic.

The fastest way to mastery is the tightest loop: write a little, `tyc check`, read the diagnostic, fix it, repeat. The compiler is patient and it's always right. Welcome to Typhon.

**Back to:** [the lesson index](README.md).
