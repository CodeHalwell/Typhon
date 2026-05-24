# Typhon example suite

A progressive tour of the Typhon language, from `hello, world` to a multi-agent LLM
system. Every example is a real-world-shaped snippet, not a contrived demo. Each
file is standalone — read top-to-bottom, or jump to the topic you care about.

Each numbered directory contains one descriptively-named `.ty` file (e.g.
`01-hello-world/hello.ty`, `33-pytorch-tensors/pytorch_tensors.ty`) **and its
emitted `.py` companion** (`01-hello-world/hello.py`,
`33-pytorch-tensors/pytorch_tensors.py`). The `.py` files are checked in so
you can see exactly what each Typhon construct lowers to without running
`tyc build` yourself. They were generated with `tyc build`; if you edit the
`.ty` and rebuild, the matching `.py` is the artefact you should diff
against.

> Tip: stdlib-only examples (01–14, 16–18, 20, 23–25, 48–67) run with a
> bare `python build/main.py` after `tyc build`, because Typhon emits a
> `typhon_runtime/` package next to the build output. The checked-in `.py`
> alongside each `.ty` needs that same runtime on its `sys.path` — copy
> the example into a `tyc init` playground and rebuild for the cleanest
> way to actually execute it.

The suite is **not** a single buildable Typhon project — each example has
its own dependency footprint (one needs `pytorch`, the next needs `redis`,
the next needs `anthropic`…). The recommended way to run any example is
to scaffold a fresh project, drop the example in, install its deps, and
build:

```bash
# pick any example, e.g. 01-hello-world
tyc init playground
cp examples/01-hello-world/hello.ty playground/src/main.ty
cd playground
# install whatever the example imports (see top of the .ty file)
tyc build
python build/main.py
```

> **Heads-up on third-party imports.** `tyc check` resolves every
> imported module against `[dependencies]` in `typhon.toml` and rejects
> unknown names — that's correct behaviour, but it bites if you only
> copy the `.ty` file into a fresh `tyc init` playground. Add the
> example's imports to `[dependencies]` (e.g. `tyc add httpx`,
> `tyc add anthropic`) before running `tyc check` / `tyc build`. Name
> resolution is enough to satisfy `tyc check`; you only need
> `tyc sync` / a `pip install` to *run* the emitted Python.

Example 47 ships as a real multi-file project with its own
`typhon.toml`; see `47-mini-app/README.md` for build/run steps.

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

### Language additions (48)

| # | Topic | Highlights |
|---|---|---|
| 48 | `newtype` IDs | nominal aliases, `UserId`/`PostId`/`Email`, escape-upward rule, `tyc::newtype_violation` |

Example 48 demonstrates `newtype`; the companion v0.3.0 features (`freeze let`, `pub`) are covered in depth in [guide 10](../docs/guides/10-advanced-features.md) and don't yet have dedicated example dirs.

### Algorithms & patterns (49–68) — stdlib-only programming exercises

Twenty additional examples that exercise the Typhon language across a
broader set of common coding patterns. Every one of these compiles, builds,
and runs against the standard library — no third-party install needed.

| # | Topic | Highlights |
|---|---|---|
| 49 | Fibonacci & memoisation | `@memo`, `@pure`, recursive closures |
| 50 | Linked list | generic sealed union (`Cons[T] \| Nil`), exhaustive `match`, `while` loops |
| 51 | Binary search tree | recursive sealed-union tree, `insert`/`contains`/`in_order` |
| 52 | Stack & queue | generic classes, `Result[T, Empty]`, `collections.deque` |
| 53 | Sorting algorithms | quicksort, mergesort, insertion sort with comprehensions |
| 54 | Graph traversal | adjacency map, BFS via deque, DFS via stack |
| 55 | Word frequency | regex tokenise, `Counter`, `@pure` predicate |
| 56 | State machine | sealed-union `State`, transition function, exhaustive matching |
| 57 | Iterators & generators | `yield`, generic `take[T]`, infinite naturals, windowed/chunked |
| 58 | Context managers | `@contextmanager`, `Iterator[T]`, timing & indentation blocks |
| 59 | Matrix operations | type aliases, `Result[Matrix, ShapeError]`, transpose & matmul |
| 60 | Caesar cipher | `@pure` cipher, brute-force breaker, scoring |
| 61 | Tic-tac-toe | nested sealed unions, board rendering, winner detection |
| 62 | Priority queue | generic class wrapping `heapq`, `Result` returns |
| 63 | Event bus | `Callable[[Event], None]`, closures over mutable state |
| 64 | URL router | pattern matching with `:param`, `Result[str, NoMatch]` |
| 65 | INI parser | line-by-line parsing with `Result[Ini, ParseError]` |
| 66 | Rate limiter | token bucket with monotonic clock |
| 67 | Trie | prefix tree, recursive collect, `words_with_prefix` |
| 68 | JSON-RPC builder | `newtype RequestId`, `unsafe:` boundary, sealed `Response` union |

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
