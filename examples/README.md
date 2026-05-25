# Typhon example suite

A curated tour of the Typhon language, kept deliberately tight: every
small example earns its place by showing a feature you can't read off
the cheat sheet, and every "app" under [`apps/`](apps/) is a multi-file
production-shaped project that stresses the language end-to-end.

The suite was pruned in May 2026 — the old `12-math-operations`,
`13-dates-and-times`, basic sorting / stack / queue exercises and the
PDF / image / streaming demos were removed because they didn't surface
anything you couldn't see in the foundations or the apps. What's left
is the high-signal subset.

Each numbered directory contains one descriptively-named `.ty` file
(e.g. `01-hello-world/hello.ty`) **and its emitted `.py` companion**
(`01-hello-world/hello.py`). The `.py` files are checked in so you can
see exactly what each Typhon construct lowers to without running
`tyc build` yourself.

> Tip: stdlib-only examples run with a bare `python build/main.py`
> after `tyc build`, because Typhon emits a `typhon_runtime/` package
> next to the build output. The checked-in `.py` alongside each `.ty`
> needs that same runtime on its `sys.path` — copy the example into a
> `tyc init` playground and rebuild for the cleanest way to actually
> execute it.

```bash
# pick any example, e.g. 01-hello-world
tyc init playground
cp examples/01-hello-world/hello.ty playground/src/main.ty
cd playground
tyc build
python build/main.py
```

> **Heads-up on third-party imports.** `tyc check` resolves every
> imported module against `[dependencies]` in `typhon.toml` and rejects
> unknown names. Add the example's imports to `[dependencies]` (e.g.
> `tyc add httpx`, `tyc add anthropic`) before running `tyc check` /
> `tyc build`.

Example 47 ships as a real multi-file project with its own
`typhon.toml`; see [`47-mini-app/README.md`](47-mini-app/README.md) for
build/run steps. The full production-shaped apps live under
[`apps/`](apps/).

## The lineup

### Foundations (1–10) — the language itself

| # | Topic | Highlights |
|---|---|---|
| 01 | Hello, world | first program, `let`, `-> None` |
| 02 | Variables & types | `let` vs `mut`, primitives, `T?` |
| 03 | Control flow | `if`, `while`, `for`, comprehensions |
| 04 | Collections | `list`, `dict`, `tuple`, `set`, slicing |
| 05 | Functions & generics | params, returns, `[T]`, default args |
| 06 | Classes & models | `class`, `impl`, `model`, `frozen`, `extend` |
| 07 | Error handling | `Result[T, E]`, `?`, `with`-chains |
| 08 | Sealed unions & match | `type X = A \| B`, exhaustive `match` |
| 09 | Interfaces | structural typing, `Protocol` emit |
| 10 | Pipes & guards | `\|>`, `guard ... else: return` |

### Daily-driver stdlib (15, 17, 20, 21)

| # | Topic | Highlights |
|---|---|---|
| 15 | Comptime config | `comptime let`, `env()`, build constants |
| 17 | File I/O — JSON | parsing into models, validation |
| 20 | Logging | structured logs, levels, formatters |
| 21 | CLI tool | argparse-style, subcommands |

### Async & web (23, 24, 28)

| # | Topic | Highlights |
|---|---|---|
| 23 | Async basics | `async def`, `await`, error propagation |
| 24 | Async gather & go | parallel awaits, fire-and-forget |
| 28 | FastAPI server | endpoints, pydantic models, deps |

### Numeric & ML (29, 33)

| # | Topic | Highlights |
|---|---|---|
| 29 | NumPy arrays | vectorised ops, broadcasting, linalg |
| 33 | PyTorch tensors | tensor ops, autograd, device |

### LLMs and agents (38, 40, 43)

| # | Topic | Highlights |
|---|---|---|
| 38 | Anthropic — basic call | client, system+user messages |
| 40 | LLM tool use | tool schemas, dispatch, results |
| 43 | Agent framework | tool registry, ReAct-style loop |

### Putting it together (47)

| # | Topic | Highlights |
|---|---|---|
| 47 | Mini app — research assistant | multi-file project: API, DB, LLM, agent |

### Language features (48)

| # | Topic | Highlights |
|---|---|---|
| 48 | `newtype` IDs | nominal aliases, `UserId`/`PostId`/`Email`, escape-upward rule, `tyc::newtype_violation` |

### High-signal algorithms & patterns (50, 56, 57, 58, 68)

| # | Topic | Highlights |
|---|---|---|
| 50 | Linked list | generic sealed union (`Cons[T] \| Nil`), exhaustive `match`, `while` loops |
| 56 | State machine | sealed-union `State`, transition function, exhaustive matching |
| 57 | Iterators & generators | `yield`, generic `take[T]`, infinite naturals, windowed/chunked |
| 58 | Context managers | `@contextmanager`, `Iterator[T]`, timing & indentation blocks |
| 68 | JSON-RPC builder | `newtype RequestId`, `unsafe:` boundary, sealed `Response` union |

### Large multi-file apps

See [`apps/README.md`](apps/README.md) for the fifteen production-shaped
multi-file projects (task scheduler, trading engine, ML orchestrator,
event-sourced banking, web crawler, GraphQL server, game ECS,
mini-compiler, search engine, distributed KV, real-time game server,
static site generator, vector DB, API gateway, stream processor) — each
with its own `typhon.toml`, `src/` tree, and run recipe.

### Bonus

| Path | Topic | Highlights |
|---|---|---|
| `testing/` | pytest | `Result`-aware assertions, parametrised tests |

## Conventions used in this suite

- Each example is a standalone runnable program with a `main()` entrypoint.
- Errors that the caller might want to handle are returned via `Result[T, E]`;
  truly exceptional conditions still raise.
- `class` is preferred for internal types, `model` for anything crossing a
  trust boundary (HTTP payloads, env, file formats).
- Long-running examples (`fastapi`, `pytorch`, etc.) are written so
  they can run end-to-end on a laptop with toy data.
- Examples that hit a paid API (Anthropic) read the key from an env var so
  they fail loudly if you haven't set it.
