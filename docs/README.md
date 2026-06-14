# Typhon Documentation

This directory holds the design and reference docs for **Typhon**, a statically-typed superset of Python that compiles to clean CPython 3.13+.

## Start here

- **[Long-term plan](long-term-plan.md)** — the harmonised implementation plan. Single canonical source for goals, architecture, language design, roadmap, and risks. Read this first.
- **[Zero to Hero](zero-to-hero/README.md)** — a ten-lesson path from install to a capstone program. Start here if you want a fast, end-to-end tour before diving into the focused guides.
- **[Programming guides](guides/README.md)** — progressive walk through the language, from hello-world to advanced features. Start here if you want to *write* Typhon code, not just understand its design.

## Focused references

The sub-docs below are extracted from the long-term plan for easier navigation. The plan itself is the source of truth; these are entry points.

| Doc | Covers |
|-----|--------|
| [architecture.md](architecture.md) | Compiler pipeline, crate layout, toolchain choices |
| [language.md](language.md) | Type system, error handling, async, `let`/`mut`, comptime, readability features |
| [cli.md](cli.md) | The `tyc` binary and its subcommands |
| [configuration.md](configuration.md) | `typhon.toml` reference (incl. `[checker]`) |
| [cheatsheet.md](cheatsheet.md) | 30-second syntax refresher (also `tyc cheatsheet`) |
| [install.md](install.md) | Installing the `tyc` binary (macOS / Linux / Windows) |
| [vm.md](vm.md) | The in-process tree-walking VM behind `tyc run` |
| [ty-integration.md](ty-integration.md) | The `tyc ty` / `[checker] external = "ty"` typeshed checker |
| [diagnostics/](diagnostics/README.md) | One page per `tyc::` diagnostic code |
| [roadmap.md](roadmap.md) | Phased delivery plan |
| [risks.md](risks.md) | Risks and mitigations |
| [prior-art.md](prior-art.md) | Languages and tools Typhon learns from |

## Status

**Phases 0–3 are substantially complete.** See [roadmap.md](roadmap.md) for the full feature list and the project [README](../README.md) for build instructions.
