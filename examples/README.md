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
>
> "Stdlib-only" means *no dependency the example imports by name*. Two
> foundations examples still need `pydantic` installed to **run**,
> because `model X:` lowers to a `pydantic.BaseModel` subclass:
> `06-classes-and-models` and `17-file-io-json`. Both type-check and
> build fine without it. Everything else in 01–10, 15, 20, 21, 23, 24,
> and 48–68 runs against a bare CPython 3.13.

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

### Language features (48, 49, 59, 60)

| # | Topic | Highlights |
|---|---|---|
| 48 | `newtype` IDs | nominal aliases, `UserId`/`PostId`/`Email`, escape-upward rule, `tyc::newtype_violation` |
| 49 | Enums | `enum` keyword (v0.11), `enum.auto()` numbering, explicit values + resume, `match` on members, `.name`/`.value` |
| 59 | Boundary casts | `try_result` (v0.15) exception→`Result`, `as!` (v0.14/v0.15) sound runtime-checked cast at an untyped JSON boundary |
| 60 | `rescue` boundaries | `rescue` (v1.0-alpha) lambda-free exception→`Result`: postfix `EXPR rescue e: ERR` and block `rescue e: ERR:` over a suite |

### High-signal algorithms & patterns (50, 56, 57, 58, 68)

| # | Topic | Highlights |
|---|---|---|
| 50 | Linked list | generic sealed union (`Cons[T] \| Nil`), exhaustive `match`, `while` loops |
| 56 | State machine | sealed-union `State`, transition function, exhaustive matching |
| 57 | Iterators & generators | `yield`, generic `take[T]`, infinite naturals, windowed/chunked |
| 58 | Context managers | `@contextmanager`, `Iterator[T]`, timing & indentation blocks |
| 68 | JSON-RPC builder | `newtype RequestId`, `try_result` + `as!` boundary parsing, sealed `Response` union |

### Large multi-file apps

See [`apps/README.md`](apps/README.md) for the fifteen production-shaped
multi-file projects (task scheduler, trading engine, ML orchestrator,
event-sourced banking, web crawler, GraphQL server, game ECS,
mini-compiler, search engine, distributed KV, real-time game server,
static site generator, vector DB, API gateway, stream processor) — each
with its own `typhon.toml`, `src/` tree, and run recipe.

### Programs that are meant to fail

Everything above shows Typhon working. [`errors/`](errors/) shows it saying
**no** — 71 deliberately broken programs, each with a header comment naming the
diagnostics it produces, explaining the rule behind them, and showing the fix.
They cover 56 of the 87 `tyc::` codes, from single-rule demos through
realistic multi-error files, plus a
[`12-known-gaps/`](errors/12-known-gaps/) directory recording programs that
compile clean today and still fail at runtime.

Each file's declared diagnostics are asserted on every `cargo test --workspace`
by `tyc/crates/tyc/tests/error_examples.rs`, so a diagnostic that stops firing
or changes severity breaks the build. See [`errors/README.md`](errors/README.md).

### VM ↔ CPython parity

[`parity/`](parity/) holds output-deterministic programs that must behave
**identically** under `tyc run` (the in-process VM) and `tyc build` + CPython —
the drop-in guarantee `docs/vm.md` makes. `parity/divergent/` holds the
confirmed exceptions, each documenting both outputs and which one is correct.
Both directions are asserted by `tyc/crates/tyc/tests/parity_corpus.rs`.

### Bonus

| Path | Topic | Highlights |
|---|---|---|
| `testing/` | pytest | `Result`-aware assertions, parametrised tests |
| `errors/` | diagnostics | 71 programs that fail on purpose, one per rule |
| `parity/` | VM parity | 18 programs asserted identical under both execution paths |

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
