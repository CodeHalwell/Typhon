# Friction encountered building `08-mini-compiler`

This app deliberately stresses recursive sealed unions (`Expr` has 13
variants, 8 self-recursive), cross-module recursive class graphs
(`Env` ↔ `VFn` ↔ `Value`), and four-stage `Result` composition (lex →
parse → check → eval, three distinct error types).

Round 1 (apps 01–05) produced 12 findings documented in
`../TYPHON_FEEDBACK.md`. Most of them re-fired in this app and were
worked around preemptively:

- **#1 (cross-module variant→union upcast)** — fired for every Token,
  Expr, Ty and Value construction outside its defining module. Added
  ~40 `make_xxx() -> Union` factories across `tokens.ty` (9), `ast.ty`
  (13), `types.ty` (6), `values.ty` (6).
- **#5 (`missing_return` after exhaustive match)** — added
  `raise RuntimeError("unreachable")` after ~22 exhaustive matches
  across `tokens.ty`, `ast.ty`, `types.ty`, `values.ty`, `eval.ty`
  and `parser.ty`. The `_values_equal` function alone has five.
- **#6 (per-arm `let` shadow)** — every arm-local binding got a
  variant-specific suffix (`av_i`, `bv_f1`, `bv_f2`, `sp_int`, `sp_kw`,
  `sp_id`, etc.) so sibling arms can't collide.
- **#9 (pattern positional arity)** — every variant pattern was
  audited to match its dataclass's exact field count; ExLet/ExFn/
  ExIf/ExBinOp all carry a `span` field that has to appear in the
  pattern even when unused.

The new friction surfaced below is specific to deeper sealed-union
graphs and `Result`-chain composition across more than two stages.

---

## 1. `with`-chain over four stages with three different error types
       has no obvious shape (severity: MEDIUM)

Code that felt awkward:
```ty
# I wanted to write something like:
with toks   = tokenize(src)?,
     ast    = parse(toks)?,
     ty     = check(ast, env)?,
     value  = eval_program(ast)?:
    return Ok(value=(ty_label(ty), value_to_str(value)))
```

…but `tokenize`, `parse`, `check`, `eval` each return a *different*
error type (`LexError`, `ParseError`, `TypeError`, `EvalError`). A
`with`-chain requires every `?` to propagate into the same `E`.
TYPHON_FEEDBACK Round 1 didn't hit this because each of the five apps
threaded a single error type through its pipeline (`PipelineError`
across all stages in the ML orchestrator, for example).

Workaround applied (in `main.ty`):
```ty
def _do_run(src: Source) -> Result[tuple[str, str], StageReport]:
    match tokenize(src):
        case Err(le): return Err(error=_report_lex_err(le))
        case Ok(tokens):
            match parse(tokens):
                case Err(pe): return Err(error=_report_parse_err(pe))
                case Ok(ast):
                    match check(ast, empty_type_env()):
                        case Err(te): return Err(error=_report_type_err(te))
                        case Ok(prog_ty):
                            match eval_program(ast):
                                case Err(ee): return Err(error=_report_eval_err(ee))
                                case Ok(v): return Ok(value=(ty_label(prog_ty), value_to_str(v)))
                            raise RuntimeError("unreachable")
                    raise RuntimeError("unreachable")
            raise RuntimeError("unreachable")
    raise RuntimeError("unreachable")
```

A four-deep nest of `match` plus four `raise RuntimeError("unreachable")`
lines is verbose and gets unwieldy at one more stage. The natural
fix would be a `with ... map_err = lambda ...:` form, e.g.

```ty
with toks   = tokenize(src).map_err(_report_lex_err)?,
     ast    = parse(toks).map_err(_report_parse_err)?,
     ty     = check(ast, env).map_err(_report_type_err)?,
     value  = eval_program(ast).map_err(_report_eval_err)?:
    return Ok(value=(ty_label(ty), value_to_str(value)))
```

Why this is a weakness: production pipelines almost always have
heterogeneous error types per stage and want to lift each into a
common report; without `map_err`/`map`/`and_then` on `Result`,
`with`-chains are limited to homogeneous-error pipelines and large
apps fall back to deeply nested `match`.

---

## 2. `let` declared without an initializer (then assigned in `case` arms)
       is rejected (severity: HIGH within this codebase, MEDIUM overall)

Code that broke:
```ty
let sp_if: Span      # split declaration, assigned in match arms below
match if_kw_res:
    case Err(e): return Err(error=e)
    case Ok(sp_v): sp_if = sp_v
```

This pattern is the *natural* way to thread a `Result` when you want
to keep the success value in the outer scope without continuing inside
the `Ok` arm. Typhon's `let` requires an initializer, so the only
shapes that work are:

```ty
# (a) chain inside the Ok arm — 5 stages deep ⇒ 5 levels of nesting
match if_kw_res:
    case Ok(sp_v):
        match next_thing(sp_v):
            case Ok(...): ...
# (b) use `?` propagation, but only if the inner type is a Result and
# the caller's E type matches
let sp_if: Span = _expect_keyword(p, "if")?
```

Workaround applied: rewrote the Pratt parser to use `?` propagation
everywhere (`_expect_keyword(p, "if")?`), which works *because every
parse helper returns the same `Result[_, ParseError]`*. But for the
main pipeline (#1 above) where errors differ per stage, `?` doesn't
apply, so deep nesting is the only option.

Why this is a weakness: split declarations are the textbook
"thread one value out of a match" pattern in Rust (`let x; match f() {
Ok(v) => x = v, Err(e) => return Err(e) }`). Without them, every
multi-stage `Result` consumer either has to be uniform-error (`?`) or
deeply nested (`match`-tower).

---

## 3. Recursive sealed union forces `expr_span()` style re-dispatch
       at every callsite that just wants a span (severity: LOW)

The Pratt parser computes spans by joining the start of one sub-expr
to the end of another. Every call site has to do:

```ty
let sp: Span = _join_span(expr_span(lhs), expr_span(rhs))
```

`expr_span` is a 13-arm exhaustive match that pulls `.span` out of the
right variant. It would be much cleaner if Typhon let sealed-union
variants declare a *shared field*, e.g.:

```ty
pub type Expr where each variant has span: Span = ExInt | ExFloat | ...
# then e.span would just work
```

Why this is a weakness: every recursive AST in real-world languages
needs uniform field access on the union (span, type-tag, source-line).
Without it, the codebase has 13-arm dispatch functions everywhere
(`expr_span`, `expr_label` in `ast.ty`, `token_span`, `token_label`
in `tokens.ty`), which double the size of the otherwise-tiny modules.

---

## 4. `frozen` placeholder-field workaround for nullary variants
       (severity: LOW)

Variants like `TyInt`, `TyUnit`, `VUnit`, `TyBool` semantically carry
no data — they're singletons. But `pub class TyInt frozen:` with an
empty body doesn't appear to be parseable, and you can't write
`pub class TyInt frozen pass` either, so every nullary variant gets:

```ty
pub class TyInt frozen:
    placeholder: int = 0
```

…and every pattern over them has to remember to write `case TyInt(_):`
(1 pattern) rather than `case TyInt():` (0 patterns). That's
contradictory with the existing apps that wrote `case JobPending():`
for a no-field variant (03-ml-orchestrator) — implying empty-body
classes *do* work, but I couldn't get them past the parser here
without the placeholder. Possible inconsistency between
`pub class X frozen:` and `pub class X:`.

Why this is a weakness: nullary variants are the cleanest sealed-union
shape (`Option.None`, `Result.Empty`, `Unit`) and they need a
zero-friction syntax — either `pub class TyInt frozen: pass` or
`pub frozen class TyInt` should be enough.

---

## 5. Recursive class graph in one file works, but the workaround is
       brittle (severity: LOW)

`VFn` holds an `Env`, and `Env.bindings: dict[str, Value]` includes
`VFn`. Defining both in `values.ty` works because Typhon can resolve
forward references inside a single module. But the obvious decomposition
— `values.ty` for `Value`, `env.ty` for `Env` — would need each module
to import the other, which Round 1 noted is brittle for sealed-union
variants. I deliberately kept them in one file rather than discover
whether `values.ty` ↔ `env.ty` cyclic imports work.

Why this is a weakness: real interpreters often want to split values
from environments; the language should make module-level cycles
expressible without forcing one big file.

---

## Summary

| Round 1 finding hit | Where it bit |
|---|---|
| #1 variant→union upcast | every Token, Expr, Ty, Value construction outside defining module (~40 factories added) |
| #5 missing_return after exhaustive match | ~22 `raise RuntimeError("unreachable")` lines across 6 files |
| #6 per-arm let shadow | renamed all arm-local bindings with variant suffixes |
| #9 pattern positional arity | every `case` pattern padded to dataclass field count |

New findings 1–5 above are specific to compiler-style code:
heterogeneous-error pipelines (#1), split-declaration `let` (#2),
shared-field access on unions (#3), nullary variants in `frozen`
classes (#4), and cross-module recursive class graphs (#5).
