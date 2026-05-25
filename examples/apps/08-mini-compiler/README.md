# 08 — Mini Compiler

A small expression-language front-end (lexer + Pratt parser) plus a type
checker and tree-walking interpreter, written end-to-end in Typhon.

The point of this app is to stress **deeply recursive sealed unions**:
the AST `Expr` has 13 variants, eight of which recursively hold other
`Expr` nodes, and every stage of the pipeline (`tokenize`, `parse`,
`check`, `eval`) returns its own `Result[T, E]`. Composing those stages
with `with`-chains and matching on the 13-variant union in three separate
passes (span lookup, type check, evaluation) is exactly the kind of
workload that exposes friction in a structurally-typed language.

## The toy language

```
let x = 1 + 2 * 3
let g = fun(a: int, b: int) -> int { a + b }
let r = g(10, 5)
if r > 0 then "pos" else "neg"
print(r)
```

Features: int/float/string/bool literals, identifiers, `+ - * /`
arithmetic, `< > <= >= == !=` comparisons, function literals
(`fun(p: T, ...) -> T { body }`), function application, `if/then/else`,
`let` bindings, `print`, line comments starting with `#`.

## Run

```bash
cd examples/apps/08-mini-compiler
tyc check src/
tyc build
python build/main.py
```

`main.ty` runs four sample programs through tokenize → parse → check →
eval and prints each stage's output (or the first stage that errored,
with the offending span).

## Typhon features exercised

- **Recursive sealed unions**: `Expr` has 13 variants (8 self-recursive).
  Every match arm carries the right number of positional patterns.
- **Cross-module sealed unions**: `Expr` (lang_ast.ty), `Token` (tokens.ty),
  `Ty` (lang_types.ty), `Value` (values.ty) are all consumed across modules,
  so every variant has a factory in its defining module to bridge the
  variant→union upcast gap.
- **Recursive class graph**: `Env` holds `dict[str, Value]`, and `VFn`
  holds an `Env`. Both live in `values.ty` to keep the cycle in one file.
- **`Result[T, E]` + `?` + `with`-chain composition**: `main.ty`
  threads four stages with three different error types through a single
  `with` block per program.
- **`impl` for state machines**: `Parser` carries a mutable `pos` and
  every method on it returns `Result` so the Pratt parser is monadic.
- **`frozen` value classes**: `Span`, `Param`, every `Ty*` and `V*`
  variant are frozen — they're value types and never mutated.
- **`pub` exports + factories everywhere**: every sealed-union
  constructor goes through a `make_xxx() -> Union` factory in its
  defining module.

## Layout

```
src/
  main.ty            # four sample programs, end-to-end
  frontend/
    source.ty        # Span, Source, line/col helpers
    tokens.ty        # Token sealed union (9 variants) + factories
    lexer.ty         # Source -> Result[list[Token], LexError]
    lang_ast.ty      # Expr sealed union (13 variants) + factories
                     # (renamed from ast.ty — `ast` shadows the stdlib, see TYPHON_FEEDBACK R2-4)
    parser.ty        # tokens -> Result[Expr, ParseError] via Pratt
  middle/
    lang_types.ty    # Ty sealed union, TypeEnv, check()
                     # (renamed from types.ty — same R2-4 reason)
  runtime/
    values.ty        # Value sealed union, Env (recursive via VFn)
    eval.ty          # eval_expr(): Expr × Env -> Result[Value, EvalError]
```

Each subpackage's `__init__.ty` is a single `pub *` marker so the
build pipeline aggregates the package's surface automatically (`from
frontend import Span, Expr, Token, …` resolves through the facade
without the consumer having to know which sibling file holds each
name).

See `FRICTION.md` for any new Typhon friction this app turned up beyond
the Round 1 findings in `../TYPHON_FEEDBACK.md`.
