# Typhon: Zero to Hero

A ten-lesson path from "never seen Typhon" to "comfortable shipping it." Each lesson is short, self-contained, and ends with a small exercise. Work through them in order the first time — every lesson builds on the last.

> These lessons are the express train; the [numbered programming guides](../guides/README.md) are the scenic route with one stop and a deeper dive per feature. Where any lesson and the [language reference](../language.md) disagree, the reference wins — and where the reference and `tyc check` disagree, **the compiler wins.** Run it often.

Every code block tagged `python` is Typhon source (a `.ty` file). Most snippets are lifted straight from the [`examples/`](../../examples/) corpus, so they compile as-is.

## The path

| # | Lesson | What you'll learn |
|---|--------|-------------------|
| 1 | [Getting started](lesson-01-getting-started.md) | What Typhon is, install `tyc`, your first program, and the eight rules that define the language |
| 2 | [Values and types](lesson-02-values-and-types.md) | `let` vs `mut`, non-nullable types, the `T?` optional form, and flow narrowing |
| 3 | [Functions, collections, and control flow](lesson-03-functions-and-control-flow.md) | Annotated functions, generics, `list`/`dict`/`tuple`, comprehensions, loops |
| 4 | [Classes, models, and enums](lesson-04-classes-models-enums.md) | The class forms, fields, and why methods live in `impl` blocks |
| 5 | [Error handling with `Result`](lesson-05-error-handling.md) | `Result[T, E]`, `Ok`/`Err`, the `?` operator, `with`-chains, `try_result` |
| 6 | [Sealed unions and `match`](lesson-06-sealed-unions-and-match.md) | `type` unions, exhaustive matching, distributing methods over variants |
| 7 | [Generics and interfaces](lesson-07-generics-and-interfaces.md) | PEP 695 generics and structural `interface`s |
| 8 | [Domain modelling](lesson-08-domain-modelling.md) | `newtype` IDs, `freeze let` constants, and `pub` visibility |
| 9 | [Async and concurrency](lesson-09-async-and-concurrency.md) | `async`/`await`, `gather:` blocks, and `go` spawn |
| 10 | [Boundaries, power tools, and a capstone](lesson-10-boundaries-and-capstone.md) | `unsafe`/`as!`/`.dty`, `comptime`/`lazy`/pipes/guards, the `tyc` workflow, and a program that ties it all together |

## How each lesson is shaped

1. **The shortest example that demonstrates the feature.**
2. **What the compiler is checking** — the rules that make it safe.
3. **Common mistakes** — diagnostics you'll actually see from `tyc check`.
4. **Try it** — a small exercise to make the idea stick.

The fastest way to learn Typhon is the tightest loop: write a little, run `tyc check`, read the diagnostic, fix it, repeat. The compiler is patient and it's always right.
