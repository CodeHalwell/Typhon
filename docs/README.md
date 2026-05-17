# Typhon Documentation

This directory holds the design and reference docs for **Typhon**, a statically-typed superset of Python that compiles to clean CPython 3.13+.

## Start here

- **[Long-term plan](long-term-plan.md)** — the harmonised implementation plan. Single canonical source for goals, architecture, language design, roadmap, and risks. Read this first.
- **[Programming guides](guides/README.md)** — progressive walk through the language, from hello-world to advanced features. Start here if you want to *write* Typhon code, not just understand its design.

## Focused references

The sub-docs below are extracted from the long-term plan for easier navigation. The plan itself is the source of truth; these are entry points.

| Doc | Covers |
|-----|--------|
| [architecture.md](architecture.md) | Compiler pipeline, crate layout, toolchain choices |
| [language.md](language.md) | Type system, error handling, async, `val`/`var`, comptime, readability features |
| [cli.md](cli.md) | The `tyc` binary and its subcommands |
| [configuration.md](configuration.md) | `typhon.toml` reference |
| [roadmap.md](roadmap.md) | Phased delivery plan |
| [risks.md](risks.md) | Risks and mitigations |
| [prior-art.md](prior-art.md) | Languages and tools Typhon learns from |

## Status

**Phases 0–3 are substantially complete.** See [roadmap.md](roadmap.md) for the full feature list and the project [README](../README.md) for build instructions.
