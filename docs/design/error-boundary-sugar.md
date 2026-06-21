# Design note: lambda-free exception boundaries (`rescue`)

**Status:** postfix + block `rescue` **implemented**, error-type check **closed** · **Branch:** `claude/typhon-error-handling-x8oau5` · **Date:** 2026-06-21

## Implementation status

- **Postfix `rescue` (statement-tail) — shipped.** `EXPR rescue NAME: ERR` lowers
  in `tyc-syntax::preprocess::expand_rescue` (folded into `expand_question_ops`,
  so it reaches `check`, `run`/VM, `build`, `fmt`, and the LSP) to
  `try_result(lambda: EXPR, lambda NAME: ERR)?`.
- **Block `rescue` — shipped.** `rescue NAME: ERR:` over a suite lowers in
  `expand_rescue_blocks` (same entry point, runs first) to a `try` /
  `except Exception as NAME: return Err(ERR)`. It emits a real `Err(...)`, so the
  checker's `return Err(...)` error-type check validates `ERR` against the
  function's declared error type. Runs to a fixpoint, so nested blocks expand.
- **Error-type check — closed.** The mapper's error type now flows through `?`
  and fires `tyc::result_error_mismatch` on a mismatch. The last hole was that
  f-strings inferred as `Unknown` (no `Expr::FString` arm in `infer_expr_ctx`),
  so an f-string mapper produced `Result[T, Unknown]`; f-strings now infer as
  `str`. This also fixed `let x: int = f"{n}"` slipping past the checker.
- Verified end-to-end for both forms: `tyc check` passes, the VM and compiled
  CPython paths produce identical output, they compose with `as!`, `tyc fmt`
  round-trips them, mismatched error types are rejected, and `rescue` in a
  non-`Result` function is rejected. Unit + checker tests added; the full
  workspace suite and the whole `examples/` + `examples/apps/` corpus re-check
  clean.
- **Not implemented (deferred):** an inline/mid-expression postfix `rescue`, the
  non-propagating `catch` sibling, per-exception-type filters, and multi-arm
  blocks (the point at which the block form would re-derive `try/except`).

---


## Summary

Typhon's *internal* error model is already simpler than Python's `try/except`:
`Result[T, E]` + `?` + `with`-chains carry failure as values, with clean stack
traces and exhaustive `match`. The one place exceptions still leak into source
is the **untyped Python boundary** — the point where a library call can throw and
we want to lift that into a `Result`.

Today that boundary is written one of two ways, and both read badly:

```python
# 1. Hand-written try/except shim — the thing we're trying to get away from
def load(path: str) -> Result[Config, str]:
    try:
        return Ok(parse(open(path).read()))
    except Exception as e:
        return Err(f"bad config: {e}")

# 2. try_result combinator — no try/except, but lambdas everywhere
def load(path: str) -> Result[Config, str]:
    return try_result(lambda: parse(open(path).read()), lambda e: f"bad config: {e}")
```

The lambdas are the ugly part. This note proposes a small, declarative surface —
**`rescue`** — that bridges a boundary into a `Result` with **no lambdas, no
`try`, no `except`** in the source, lowering to the machinery that already exists.

## Goal and non-goals

**Goal.** Make the common boundary case — "run this; if it throws, turn the
exception into *my* error and propagate" — a one-liner that reads top-to-bottom.

**Non-goals.**
- Not a general control-flow `try`. `rescue` only ever *produces a `Result`* (or
  propagates one). It cannot be used to resume, retry, or run arbitrary handler
  logic — that is what `match` on a `Result` is for.
- Not checked exceptions. We do not add a `raises` clause to user functions;
  fallibility stays encoded in the `Result` return type.
- Not a second error channel. `Result` remains the one and only error model;
  `rescue` is purely the *bridge* from the exception world into it.

## Proposed surface

### 1. Postfix `rescue` — catch + propagate (the 90% case)

```
EXPR rescue NAME: ERR_EXPR
```

- Evaluates `EXPR`. On success the whole form has `EXPR`'s type.
- If `EXPR` raises, the exception is bound to `NAME`, `ERR_EXPR` is evaluated,
  and `Err(ERR_EXPR)` is **propagated to the enclosing function** — exactly like
  `?`. The enclosing function must therefore return a compatible `Result[_, E]`.
- `ERR_EXPR` is a single expression (like a ternary arm), so there is no block,
  no colon-indentation ambiguity, and no lambda.

```python
def load_config(path: str) -> Result[Config, ConfigError]:
    let text: str           = open(path).read()      rescue e: NotFound(path=path)
    let data: dict[str,str] = json.loads(text) as! dict[str,str]
                                                     rescue e: BadJson(reason=str(e))
    let port: int           = int(data["port"])      rescue e: BadPort(raw=data["port"])
    return Ok(Config(host=data["host"], port=port))
```

One fallible step per line, each mapped to one error, reads straight down. No
`lambda`, no `?` needed (it's folded in), no `try`.

### 2. Block `rescue` — one catch-all over many statements

When several statements share a single catch-all mapping, a prefix block avoids
repeating `rescue e: …` on every line:

```python
def load_config(path: str) -> Result[Config, str]:
    rescue e: f"bad config: {e}":
        let data: dict[str,str] = json.loads(open(path).read()) as! dict[str,str]
        return Ok(Config(host=data["host"], port=int(data["port"])))
```

Any exception raised anywhere in the body is mapped through `ERR_EXPR` and
returned as `Err(...)`. This is the faithful, apples-to-apples replacement for a
Python `try: … except Exception as e: …` shim — but spelled as a single
error-mapping prefix with no per-exception ceremony.

### 3. `catch` — catch *without* propagating (sibling, lower priority)

Occasionally you want the `Result` *value* (to `match` on it, store it, or pass
it on) rather than bubble it up. Same syntax, different keyword, no implicit `?`:

```python
let r: Result[int, str] = int(s) catch e: f"not an int: {e}"
match r:
    case Ok(n):   ...
    case Err(msg):...
```

`EXPR rescue e: M` is exactly `(EXPR catch e: M)?`. If we only ship one keyword
in v1, ship `rescue` — `catch` can follow.

## Semantics

- **What is caught:** `Exception` (not `BaseException`), matching `try_result`
  today. `KeyboardInterrupt` / `SystemExit` propagate as normal.
- **Binding:** `NAME` binds the caught exception object within `ERR_EXPR` only.
  It does not need `let`/`mut` (it's a binding form, like a `case`/`except`
  target). `NAME` may be `_` if unused.
- **Type of `EXPR rescue e: M`:** the success type of `EXPR` (call it `T`). The
  form participates in inference like any expression, so `let x: T = … rescue …`
  and `return Ok(… rescue …)` both work.
- **Error type:** the static type of `ERR_EXPR` (call it `E`). For the postfix
  (propagating) form, `E` must be assignable to the enclosing function's declared
  error type — reusing the **exact** check `?` already performs
  (`tyc::result_error_mismatch`). Variant → sealed-union widening applies, so
  `rescue e: NotFound(...)` satisfies `Result[_, ConfigError]`.
- **Placement:** postfix `rescue` is only legal inside a `Result`-returning
  function (same rule as `?`); using it elsewhere fires `tyc::invalid_question_op`
  (or a dedicated `tyc::rescue_outside_result`). `catch` has no such restriction.
- **Comprehension carve-out:** same as `?` — rejected inside comprehensions/genexps.

## Lowering

Both forms reuse machinery that already ships, so this is sugar, not a new
runtime.

**Postfix** lowers to the existing `try_result` prelude combinator plus `?`:

```python
# source
let port: int = int(data["port"]) rescue e: BadPort(raw=data["port"])

# desugar (tyc-syntax preprocess → tyc-desugar)
let port: int = try_result(
    lambda: int(data["port"]),
    lambda e: BadPort(raw=data["port"]),
)?
```

The lambdas exist only in the lowered form the user never reads — the whole point
is that they vanish from source. (Alternatively the postfix form can lower to an
inline `try/except` with a fresh temp for marginally cleaner frames; reusing
`try_result` is the smaller change and is recommended for v1.)

**Block** lowers to a real `try/except` returning `Err` (a block can hold
statements, which a `lambda` can't):

```python
# source
rescue e: f"bad config: {e}":
    let data = json.loads(open(path).read()) as! dict[str,str]
    return Ok(Config(host=data["host"], port=int(data["port"])))

# emitted Python
try:
    data = __typhon_checked_cast__(json.loads(open(path).read()), dict[str, str])
    return Ok(Config(host=data["host"], port=int(data["port"])))
except Exception as e:
    return Err(f"bad config: {e}")
```

Emitted Python is free to use `try/except`; the design goal is only that the
**`.ty` source** is free of it.

## Diagnostics

- `tyc::rescue_outside_result` — postfix `rescue` in a non-`Result` function.
  (Or reuse `tyc::invalid_question_op` with extended help text.)
- `tyc::result_error_mismatch` — `ERR_EXPR` type not assignable to the enclosing
  function's error type. **Reused verbatim** from `?`.
- `tyc::rescue_in_comprehension` — postfix `rescue` inside a comprehension/genexp.
  (Or reuse the `?` carve-out diagnostic.)
- Advice (optional): a bare `try/except` shim that only maps to `Ok`/`Err` could
  surface a `tyc::prefer_rescue` hint pointing at this form.

## Worked comparison

```python
# ---- Python -------------------------------------------------------------
def load_config(path: str) -> dict:
    try:
        with open(path) as f:
            data = json.load(f)
        return {"host": data["host"], "port": int(data["port"])}
    except (FileNotFoundError, json.JSONDecodeError, KeyError, ValueError) as e:
        raise RuntimeError(f"bad config: {e}")

# ---- Typhon, catch-all (block rescue) — shorter than Python -------------
def load_config(path: str) -> Result[Config, str]:
    rescue e: f"bad config: {e}":
        let data: dict[str,str] = json.loads(open(path).read()) as! dict[str,str]
        return Ok(Config(host=data["host"], port=int(data["port"])))

# ---- Typhon, typed per-step (postfix rescue) — one line per failure -----
def load_config(path: str) -> Result[Config, ConfigError]:
    let text: str           = open(path).read()      rescue e: NotFound(path=path)
    let data: dict[str,str] = json.loads(text) as! dict[str,str]
                                                     rescue e: BadJson(reason=str(e))
    let port: int           = int(data["port"])      rescue e: BadPort(raw=data["port"])
    return Ok(Config(host=data["host"], port=port))
```

No `try`, no `except`, no `lambda` in any of the Typhon source.

## Implementation sketch

The pipeline is `syntax → resolve → types → analyse → desugar → emit`
(see `docs/architecture.md`). `rescue` touches:

1. **`tyc-syntax` (`preprocess.rs`)** — recognise postfix `EXPR rescue NAME: ERR`
   and the `rescue NAME: ERR:` block header, the same way `?`, `|>`, `with`-chains,
   and `gather:` are preprocessed. Postfix needs the same left-operand scanning
   `as!` already does (back to an enclosing bracket / separator / assignment /
   `return`). Bracket-, string-, and comment-aware (cf. the v0.15.2 `as!` fix).
2. **`tyc-resolve`** — bind `NAME` in the scope of `ERR_EXPR` (postfix) or the
   block body's `except` scope (block). It's a binding target, not a `let`.
3. **`tyc-types`** — type the form as `T` (success type of `EXPR`); for the
   propagating form, run the existing `?` error-type check against the enclosing
   function. Reuses `result_error_mismatch`.
4. **`tyc-desugar`** — postfix → `try_result(lambda: EXPR, lambda e: ERR)?`;
   block → `try/except` returning `Err`.
5. **`tyc-emit`** — nothing new; emits the lowered nodes.
6. **`tyc-vm`** — `try_result` and `?` already run in the VM; the block form needs
   the VM's existing `try/except` support. Should fall out for free; add a VM
   test.
7. **Docs/tests** — extend `docs/guides/06-error-handling.md`, add an
   `examples/` program, round-trip through `tyc fmt`.

Estimated surface: comparable to the `as!` cast (a preprocess + desugar + a
reused type check), which is a known-sized change.

## Open questions

1. **Keyword.** `rescue` (Ruby), `else` (Elixir `with`), `or` + binder, or a
   symbol. `rescue` reads clearly and doesn't collide with Python keywords.
   `catch` is reserved here for the non-propagating sibling.
2. **Ship `catch` too, or just `rescue`?** Recommendation: `rescue` first; add
   `catch` only if the keep-the-`Result` case shows up enough.
3. **Default caught type.** `Exception` (recommended) vs allowing an optional
   filter `EXPR rescue (json.JSONDecodeError) e: …` to discriminate by type. The
   filter is a clean extension but adds parser surface; defer to v2 unless
   demand is clear. The per-step postfix form already discriminates *by
   position*, which covers most of the need.
4. **Multi-arm block** (`rescue: … catch A as e: … catch B as e: …`) — this is
   the point where we'd be re-deriving `try/except`. Deliberately left out of v1;
   per-step postfix `rescue` covers typed discrimination without it.

## Recommendation

Ship **postfix `rescue` + block `rescue`** in one release, lowering to the
existing `try_result`/`?`/`try-except` machinery. That removes the last lambdas
and the last `try/except` from idiomatic source, makes the catch-all case
*shorter* than Python, and keeps per-step typed errors a clean one-liner — without
adding a second error channel or checked exceptions. Defer `catch`, type filters,
and multi-arm blocks until there's evidence they're needed.
