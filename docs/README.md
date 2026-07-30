# Typhon Documentation

This directory holds the design and reference docs for **Typhon**, a statically-typed superset of Python that compiles to clean CPython 3.13+.

## Start here

**Want to *learn the language*?** Start with the teaching material, not the design docs:

- **[Zero to Hero](zero-to-hero/README.md)** — a ten-lesson path from install to a capstone program. The fastest end-to-end tour.
- **[Programming guides](guides/README.md)** — a progressive, feature-by-feature walk through the language, with the emitted Python shown side-by-side.
- **[The eight rules](language.md#the-eight-rules-every-typhon-program-follows)** — the short list of what makes Typhon stricter than Python. If you read nothing else, read this.
- **[Cheat sheet](cheatsheet.md)** — the 30-second syntax refresher (also `tyc cheatsheet`).

**Want to understand the *design*?** Read the **[long-term plan](long-term-plan.md)** — the single canonical source for goals, architecture, language design, roadmap, and risks.

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
| [differential-testing.md](differential-testing.md) | The VM ↔ CPython differential gate and the opt-in knob codegen matrix |
| [diagnostics/](diagnostics/README.md) | One page per `tyc::` diagnostic code |
| [roadmap.md](roadmap.md) | Phased delivery plan |
| [risks.md](risks.md) | Risks and mitigations |
| [prior-art.md](prior-art.md) | Languages and tools Typhon learns from |

## Status

**Current release: [v1.0.0-alpha.7](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha.7).** Typhon reached its first *feature-complete* alpha in [v1.0.0-alpha](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha); the production path (`tyc build` → CPython 3.13+) is stable, and the language is additive on *correct* programs across the whole v0.3.0 → v1.0.0-alpha line. As an alpha, the surface syntax is not yet frozen. See [roadmap.md](roadmap.md) for the per-feature status, [../CHANGELOG.md](../CHANGELOG.md) for the release-by-release history, and the project [README](../README.md) for build instructions.
