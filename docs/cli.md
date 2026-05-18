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
| `tyc migrate` | Convert typed Python (`.py`) to Typhon (`.ty`): rewrites `Optional[T]`/`T \| None` → `T?`, adds `let`/`mut` to module-level annotated assigns, strips `@dataclass` decorators. |
| `tyc ty` | Build the project and run Astral's `ty` checker against the emitted Python. Requires `ty` on `PATH` (`pip install ty`). Supports `--watch` for continuous feedback. |
| `tyc repl` | Interactive Typhon evaluator. Reads `.ty` source one block at a time, compiles it through the full pipeline, and executes the result with a Python interpreter. |
| `tyc debug` | Build the project and launch the emitted Python under a debugger (default `pdb`). Thin v1 wrapper — a Typhon-native source-mapping debugger is a Phase-5 item. |
| `tyc add` / `tyc remove` / `tyc sync` | Lightweight package-manager surface over `uv`: rewrite `[dependencies]` / `[dev-dependencies]` in `typhon.toml` and run `uv sync` to install. |

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

## `tyc migrate`

Converts typed Python (`.py`) to Typhon (`.ty`) in one pass:

- `Optional[T]` / `T | None` → `T?`
- Module-level annotated assignments (`x: int = 1`) gain `let` (or `mut` when later reassigned).
- `@dataclass` decorators and their `from dataclasses import dataclass` are dropped.

```bash
# Convert a single file (writes app.ty alongside app.py):
tyc migrate src/app.py

# Preview without writing (prints to stdout):
tyc migrate --check src/app.py
```

`--check` mode is useful in CI to confirm that a `.py` file is already Typhon-compatible.

## `tyc ty`

Builds the project and runs Astral's [`ty`](https://github.com/astral-sh/ty) type checker against the emitted Python files. Requires `ty` to be installed separately (`pip install ty` or `uv tool install ty`).

```bash
# One-shot check:
tyc ty

# Write emitted Python to an explicit directory (useful in CI):
tyc ty --out build/

# Watch mode — re-runs on every .ty / .dty change:
tyc ty --watch

# Pass extra flags to `ty check` verbatim:
tyc ty -- --strict
```

| Flag | Purpose |
|------|---------|
| `--out DIR` | Write emitted Python here instead of a temp dir |
| `--ty-bin BIN` | Path to the `ty` executable (default: `ty`) |
| `--no-build` | Skip the build step; requires `--out` so the directory is known |
| `--watch` | Watch source directory and re-run on `.ty` / `.dty` changes |

## `tyc repl`

Interactive Typhon evaluator. Each prompt accumulates source, recompiles the whole session through the full Typhon pipeline, and executes the result with a Python interpreter. Only the new tail of stdout is displayed after each input.

```bash
# Start the REPL (auto-detects python3.13 / python3.12 / python3):
tyc repl

# Pre-load a .ty file as the initial session:
tyc repl --load src/lib.ty

# Use a specific interpreter:
tyc repl --python python3.13
```

**REPL commands:**

| Input | Effect |
|-------|--------|
| `:quit` | Exit the REPL |
| `:reset` | Clear the accumulated session |
| `:show` | Dump the current session source |

**Known limitations:** each prompt re-executes the entire accumulated session (pure-scratch semantics); side effects fire once per prompt. Multi-line blocks end on the first blank line. No readline/arrow-key support.

## `tyc debug`

Builds the project, then execs the configured Python debugger on the emitted entry-point. v1 is a deliberately thin wrapper — frames surface as `build/*.py` paths; pair with `tyc trace` to remap captured tracebacks back to `.ty` source via the `.py.map` sidecars.

```bash
# Step through build/main.py under pdb:
tyc debug

# Different entry point + debugger:
tyc debug --entry api.py --debugger pudb

# Forward args to the script:
tyc debug -- --verbose --port 8080
```

| Flag | Purpose |
|------|---------|
| `--entry FILE` | Entry-point file relative to the build dir (default `main.py`) |
| `--python PATH` | Python interpreter (default `python3`) |
| `--debugger MODULE` | Module to launch under `python -m` (default `pdb`; e.g. `pudb`, `ipdb`, `debugpy`) |
| `--no-build` | Skip rebuilding; assume `build/` is current |

## `tyc add` / `tyc remove` / `tyc sync`

Manage project dependencies declared in `[dependencies]` and `[dev-dependencies]` of `typhon.toml`. The commands rewrite the manifest and shell out to [`uv`](https://github.com/astral-sh/uv) for the install step; `uv` itself is not bundled, so when it is missing the manifest still updates and the command prints a clear "install uv" message.

```bash
# Add a runtime dependency (latest), then a dev dependency at a version:
tyc add requests
tyc add --dev pytest@8.2

# Remove a package:
tyc remove rich

# Materialise [dependencies] into a generated pyproject.toml and uv sync:
tyc sync

# Preview the generated pyproject.toml without writing or installing:
tyc sync --dry-run
```

`--no-sync` on `tyc add` / `tyc remove` skips the `uv` install step — useful for batching edits and running `tyc sync` once at the end.

## CI integration

`tyc check` is the recommended command for CI: it runs everything up to the analyser without emitting `.py` output, so it fails fast on type errors without producing artifacts.

## Editor integration

`tyc lsp` runs on stdio and speaks LSP. The reference VS Code extension wires it up; any LSP-aware editor can use it directly. Diagnostics, hover, and (over time) completions and code actions are exposed through the same Salsa-backed query engine the CLI uses.
