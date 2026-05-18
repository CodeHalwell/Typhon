# Typhon example suite

A progressive tour of the Typhon language, from `hello, world` to a multi-agent LLM
system. Every example is a real-world-shaped snippet, not a contrived demo. Each
file is standalone — read top-to-bottom, or jump to the topic you care about.

Each numbered directory contains one descriptively-named `.ty` file (e.g.
`01-hello-world/hello.ty`, `33-pytorch-tensors/pytorch_tensors.ty`). Compile
and run any of them with:

```bash
tyc build examples/01-hello-world/hello.ty
python build/hello.py
```

Most examples are single-file. A few projects (e.g. `47-mini-app/`) span
multiple files; build the directory instead:

```bash
tyc build examples/47-mini-app/src/
python build/main.py
```

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

### Standard library (11–22) — daily-driver tasks

| # | Topic | Highlights |
|---|---|---|
| 11 | String manipulation | parsing, formatting, `extend str:` |
| 12 | Math operations | numeric ops, stats, `@pure` `@memo` |
| 13 | Dates & times | `datetime`, parsing, arithmetic |
| 14 | Regex | matching, groups, replacement |
| 15 | Comptime config | `comptime let`, `env()`, build constants |
| 16 | File I/O — text | reading/writing, line iteration |
| 17 | File I/O — JSON | parsing into models, validation |
| 18 | File I/O — CSV | streaming, dict-rows, type-safe |
| 19 | File I/O — PDF | text extraction with `pypdf` |
| 20 | Logging | structured logs, levels, formatters |
| 21 | CLI tool | argparse-style, subcommands |
| 22 | HTTP requests | `requests`, retries, typed responses |

### Async, web, data layer (23–32)

| # | Topic | Highlights |
|---|---|---|
| 23 | Async basics | `async def`, `await`, error propagation |
| 24 | Async gather & go | parallel awaits, fire-and-forget |
| 25 | SQLite | raw SQL, transactions, typed rows |
| 26 | SQLAlchemy ORM | models, queries, sessions |
| 27 | Redis cache | string/hash ops, pipelining |
| 28 | FastAPI server | endpoints, pydantic models, deps |
| 29 | NumPy arrays | vectorised ops, broadcasting, linalg |
| 30 | Pandas cleaning | load, clean, group, aggregate |
| 31 | Matplotlib plot | line/bar/scatter, styling |
| 32 | scikit-learn | pipeline, train/test split, metrics |

### Machine learning & vision (33–37)

| # | Topic | Highlights |
|---|---|---|
| 33 | PyTorch tensors | tensor ops, autograd, device |
| 34 | PyTorch neural net | `nn.Module`, forward, init |
| 35 | PyTorch training loop | dataset, dataloader, optimiser, eval |
| 36 | Hugging Face transformer | pipeline, tokenizer, inference |
| 37 | Image processing (Pillow) | open, resize, crop, filter, save |

### LLMs and agents (38–46)

| # | Topic | Highlights |
|---|---|---|
| 38 | Anthropic — basic call | client, system+user messages |
| 39 | LLM streaming | streaming events, token deltas |
| 40 | LLM tool use | tool schemas, dispatch, results |
| 41 | LLM structured output | pydantic schema, parse + validate |
| 42 | RAG system | embed, index, retrieve, ground |
| 43 | Agent framework | tool registry, ReAct-style loop |
| 44 | Multi-agent orchestration | router + worker agents, shared state |
| 45 | Web scraper | `httpx` + `BeautifulSoup`, polite |
| 46 | Task queue worker | producer/consumer with Redis |

### Putting it together (47)

| # | Topic | Highlights |
|---|---|---|
| 47 | Mini app — research assistant | multi-file project: API, DB, LLM, agent |

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
- Long-running examples (`fastapi`, `pytorch training`, etc.) are written so
  they can run end-to-end on a laptop with toy data.
- Examples that hit a paid API (Anthropic) read the key from an env var so
  they fail loudly if you haven't set it.
