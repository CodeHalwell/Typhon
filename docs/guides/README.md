# Typhon Programming Guides

A progressive walk through the language, from your first `.ty` file to advanced features. Each guide builds on the previous one; skim in order the first time.

> The canonical design doc is [language.md](../language.md). These guides are the *teaching* surface — they show how features feel in practice. Where the two ever drift, the design doc wins.

## Reading order

| # | Guide | What you'll learn |
|---|-------|-------------------|
| 1 | [Hello, world](01-hello-world.md) | Install `tyc`, scaffold a project, write your first program, read the emitted Python |
| 2 | [Values and types](02-values-and-types.md) | `val` vs `var`, primitives, non-nullable by default, the `T?` optional form, flow narrowing |
| 3 | [Functions](03-functions.md) | Function declarations, parameter and return annotations, default arguments, the no-implicit-`Any` rule |
| 4 | [Control flow and collections](04-control-flow-and-collections.md) | `if`/`while`/`for`, `list`/`dict`/`set`/`tuple`, comprehensions, guards |
| 5 | [Classes and models](05-classes-and-models.md) | `class` (dataclass), `model` (Pydantic), `impl` blocks, `extend`, frozen instances |
| 6 | [Error handling with `Result`](06-error-handling.md) | `Result[T, E]`, `Ok`/`Err`, the `?` operator, `with`-chains |
| 7 | [Sealed unions and `match`](07-sealed-unions-and-match.md) | `type` aliases, sum types, exhaustive matching, variant guards |
| 8 | [Generics and interfaces](08-generics-and-interfaces.md) | PEP 695 generics, structural `interface`, bidirectional inference |
| 9 | [Async and concurrency](09-async-and-concurrency.md) | `async`/`await`, `gather:` blocks, `go` spawn, free-threaded mode |
| 10 | [Advanced features](10-advanced-features.md) | Pipes, `comptime`, `lazy import`, `@pure`/`@memo`, `unsafe`, `.dty` stubs |

## How each guide is structured

Every guide follows the same shape:

1. **The shortest example that demonstrates the feature.**
2. **What the compiler is checking** — the rules that make it safe.
3. **The emitted Python** — so you can predict what ships to production.
4. **Common mistakes** — diagnostics you'll actually see from `tyc check`.

## Conventions

- Typhon source is shown in fenced blocks tagged `python` (editors highlight `.ty` well enough with Python lexers — a `typhon` lexer is on the roadmap).
- Emitted Python is shown side-by-side or under a "Compiles to" heading.
- Diagnostic excerpts are quoted from `tyc check` output verbatim where possible.

When in doubt, run `tyc check src/` — the compiler is the source of truth for what the language accepts today.
