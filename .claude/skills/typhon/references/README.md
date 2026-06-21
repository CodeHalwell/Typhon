# Typhon reference examples

Curated, **compile-clean** single-file `.ty` programs, one feature area each.
Every file is lifted verbatim from the project's `examples/` corpus, so each one
type-checks under `tyc check` and runs under both `tyc run` (in-process VM) and
`tyc build && python build/main.py` on the matching release.

Read them in order the first time — they build on each other. When you need the
authoritative prose for a feature, follow the cross-reference back into the skill
(`SKILL.md`, `REFERENCE.md`, etc.).

| File | Feature area | Skill section |
|---|---|---|
| `01-hello-world.ty` | `let` / `mut`, `def … -> None`, entry point | SKILL §2, §4 Rule 1–2 |
| `02-variables-and-types.ty` | Primitives, widening, `T?`, collections | SKILL §5.1–5.3 |
| `03-control-flow.ty` | `if`/`elif`/`else`, `for`, `while`, walrus, narrowing | SKILL §4 Rule 3, §5.3 |
| `04-collections.ty` | `list`/`dict`/`set`/`tuple`, comprehensions | SKILL §5.2 |
| `05-functions-and-generics.ty` | PEP 695 generics, `Callable`, defaults | SKILL §5.4 |
| `06-classes-and-models.ty` | `class` / `frozen` / `model` / `impl`, fields | SKILL §4 Rule 4, §5.7 |
| `07-error-handling.ty` | `Result[T, E]`, `Ok`/`Err`, `?`, `with`-chains | SKILL §9 |
| `08-sealed-unions-match.ty` | `type X = A \| B`, exhaustive `match`, `impl` on union | SKILL §4 Rule 6, §5.6 |
| `09-interfaces.ty` | Structural `interface`, conformance | SKILL §5.5 |
| `10-pipes-and-guards.ty` | `\|>` pipe, `guard … else` | SKILL §3 cheat sheet |
| `11-comptime-config.ty` | `comptime let`, `env(...)`, build-time inlining | SKILL §12 |
| `12-file-io-json.ty` | `with open(...)`, `json`, resource management | SKILL §6 (`resource_not_managed`) |
| `13-async-gather-and-go.ty` | `async`/`await`, `gather:`, `go`, best-effort | SKILL §10 |
| `14-newtype-ids.ty` | `newtype`, nominal IDs, same-newtype arithmetic | SKILL §6 |
| `15-enums.ty` | `enum` keyword (v0.11.0), `enum.auto()` | SKILL §5.7 |
| `16-linked-list-generics.ty` | Recursive generic ADTs, `impl[T]` | SKILL §5.4, §5.6 |
| `17-state-machine.ty` | Sealed-union state model + transitions | SKILL §5.6 |
| `18-iterators-generators.ty` | `yield` / `yield from`, `Iterator[T]` | SKILL §11, RUNTIME |
| `19-context-managers.ty` | `@contextmanager`, `with … as` typing | SKILL §10, §5 |
| `20-boundary-casts.ty` | `as!` checked cast + `try_result` (v0.14–v0.15) | SKILL §5.10, §9 |
| `21-rescue-boundaries.ty` | `rescue` postfix + block exception boundaries (Unreleased) | SKILL §9 |

## Running an example

```bash
tyc check references/07-error-handling.ty     # type-check only
tyc run   references/07-error-handling.ty     # execute via the in-process VM
tyc build references/07-error-handling.ty     # emit build/…py + .py.map sidecars
```

## Installing this skill into another project

From the Typhon repo (or any project where `tyc` is on `PATH`):

```bash
tyc install skill          # writes .claude/skills/typhon/ into the current project
tyc install skill --force  # overwrite an existing copy
```

The installed copy includes this `references/` folder verbatim.
