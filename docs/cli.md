# CLI Reference

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Typhon ships a single binary, `tyc`, that handles every stage of the workflow. Subcommands are built with `clap` v4.

## Subcommands

| Command | Purpose |
|---------|---------|
| `tyc build` | Full pipeline: parse, check, analyse, desugar, emit, format. |
| `tyc check` | Up to analyser, no emit. Used by CI. |
| `tyc fmt` | Format `.ty` source. Wraps `ruff format` applied to a Typhon-aware pretty-printer. |
| `tyc lsp` | Run as a Language Server. |
| `tyc init` | Scaffold a new project: `typhon.toml`, `src/`, `tests/`. |
| `tyc trace` | Map a Python traceback back to Typhon source via `.py.map` files. |
| `tyc profile` | Instrument emitted code for hot-function detection (advanced, opt-in). |

## Typical workflow

```bash
# Build the compiler
cd tyc
cargo build --release

# Scaffold a new project
./target/release/tyc init myapp
cd myapp

# Format and check
tyc fmt src/
tyc check src/

# Emit Python
tyc build
```

## CI integration

`tyc check` is the recommended command for CI: it runs everything up to the analyser without emitting `.py` output, so it fails fast on type errors without producing artifacts.

## Editor integration

`tyc lsp` runs on stdio and speaks LSP. The reference VS Code extension wires it up; any LSP-aware editor can use it directly. Diagnostics, hover, and (over time) completions and code actions are exposed through the same Salsa-backed query engine the CLI uses.
