# Lesson 8 — Domain modelling

*Zero to Hero · Lesson 8 of 10*

Three features that turn "it's just an `int`" bugs into compile errors and make your module boundaries explicit: `newtype`, `freeze let`, and `pub`.

## `newtype` — nominal IDs

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

The relationship is **asymmetric**:

- A `UserId` flows freely into an `int` slot — it really *is* an `int` at runtime:
  ```python
  def double(n: int) -> int: return n * 2
  let twice: int = double(me)           # ✅ UserId → int is free
  ```
- A bare `int` will *not* satisfy a `UserId` parameter without the explicit constructor.

So you can never accidentally pass a `PostId` where a `UserId` belongs — even though both are `int`. Use newtypes for ID kinds, currency tags, internal-vs-external markers — anywhere "an `int` is an `int`" costs you debugging time. Same-newtype arithmetic preserves the type (`UserId(a) + UserId(b)` is `UserId`).

## `freeze let` — deep-immutable constants

`let` locks the *binding*; `freeze let` recursively freezes the *value* too (module level):

```python
freeze let CONFIG = {"port": 8080, "hosts": ["a", "b"]}
# CONFIG = {...}             # ❌ tyc::immutable_assign  (binding locked)
# CONFIG["port"] = 9000      # ❌ TypeError at runtime   (read-only mapping)
# CONFIG["hosts"].append(…)  # ❌ AttributeError         (the list became a tuple)
```

It converts `list → tuple`, `dict → read-only mapping`, `set → frozenset`, descending recursively. Use it for configuration tables and lookup constants you never want mutated.

## `pub` — module visibility

Mark the public surface of a module with `pub`; everything else stays internal to the file:

```python
pub let API_VERSION: str = "v1"
pub class Client:
    host: str
pub def connect(host: str) -> Client: ...

let _default_port: int = 8080      # not exported
```

When a module has any `pub` name, an `__all__` is synthesised for you, so `from mod import *`, IDEs, and doc tools all see the same public surface. `pub` stacks with every modifier — `pub class X frozen`, `pub model`, `pub newtype`, `pub freeze let`, `pub async def`, and so on.

For a package, put `pub *` in its `__init__.ty` to aggregate every sibling module's public names into the package namespace — the clean way to build a multi-file library.

## Common mistakes

**Passing a bare base value into a newtype slot:**

```python
greet(42, Email("x@y.z"))          # ❌ tyc::newtype_violation
greet(UserId(42), Email("x@y.z"))  # ✅
```

**Mixing two newtypes over the same base** is also caught — a `PostId` won't satisfy a `UserId` parameter; wrap with `UserId(post)` to cross deliberately.

**`pub *` outside `__init__.ty`** is a no-op (advice diagnostic) — it only aggregates at the package root.

## Try it

1. Define `newtype AccountId = int` and `newtype Cents = int`. Write `def transfer(src: AccountId, dst: AccountId, amount: Cents) -> None:`. Try calling it with the arguments in the wrong order using bare ints, and watch the type errors.
2. Define `freeze let LIMITS = {"free": 100, "pro": 10_000}` and try to mutate it under `tyc run`.
3. Add `pub` to one function in a module and build it — find the synthesised `__all__` in the emitted `.py`.

## What you learned

- `newtype` gives you distinct, zero-cost types over a base — asymmetric, so accidental mixing is a compile error.
- `freeze let` makes a module constant deeply immutable.
- `pub` (and `pub *` in `__init__.ty`) declares a module's public surface.

**Next:** [Lesson 9 — Async and concurrency](lesson-09-async-and-concurrency.md).
