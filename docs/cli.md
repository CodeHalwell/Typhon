# CLI Reference

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Typhon ships a single binary, `ttc`, that handles every stage of the workflow. Subcommands are built with `clap` v4.

## Subcommands

| Command | Purpose |
|---------|---------|
| `ttc build` | Full pipeline: parse, check, analyse, desugar, emit, format. |
| `ttc check` | Up to analyser, no emit. Used by CI. |
| `ttc fmt` | Format `.ty` source. Wraps `ruff format` applied to a Typhon-aware pretty-printer. |
| `ttc lsp` | Run as a Language Server. |
| `ttc init` | Scaffold a new project: `typhon.toml`, `src/`, `tests/`. |
| `ttc trace` | Map a Python traceback back to Typhon source via `.py.map` files. |
| `ttc profile` | Instrument emitted code for hot-function detection (advanced, opt-in). |

## Typical workflow

```bash
# Build the compiler
cd ttc
cargo build --release

# Scaffold a new project
./target/release/ttc init myapp
cd myapp

# Format and check
ttc fmt src/
ttc check src/

# Emit Python
ttc build
```

## CI integration

`ttc check` is the recommended command for CI: it runs everything up to the analyser without emitting `.py` output, so it fails fast on type errors without producing artifacts.

## Editor integration

`ttc lsp` runs on stdio and speaks LSP. The reference VS Code extension wires it up; any LSP-aware editor can use it directly. Diagnostics, hover, and (over time) completions and code actions are exposed through the same Salsa-backed query engine the CLI uses.
