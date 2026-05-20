# CLI Reference

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Typhon ships a single binary, `tyc`, that handles every stage of the workflow. Subcommands are built with `clap` v4.

## Subcommands

| Command | Purpose |
|---------|---------|
| `tyc build` | Full pipeline: parse, check, analyse, desugar, emit, format. Also bootstraps the Python environment — merges the owned keys (`[project] name/version/requires-python/dependencies`, plus `[dependency-groups].dev` when `[dev-dependencies]` is non-empty) into `pyproject.toml` (preserving any user-managed `[tool.*]` / other `[project]` keys) and runs `uv sync` so `.venv` is ready. `uv sync` failure is downgraded to a warning. |
| `tyc check` | Up to analyser, no emit. Used by CI. |
| `tyc fmt` | Format `.ty` source. The v1 pass collapses runs of interior whitespace, strips space after `(`/`[` and before `)`/`]`/`,`/`;`, and normalises trailing-whitespace / line-endings. Spacing around `:`, `=`, and `->` is left alone today — those need bracket-depth awareness (slice vs annotation) and are deferred. A full AST-based reprinter is a Phase-5 follow-up. |
| `tyc lsp` | Run as a Language Server. |
| `tyc init` | Scaffold a new project: `typhon.toml`, `src/`, `tests/`. |
| `tyc trace` | Map a Python traceback back to Typhon source via `.py.map` files. |
| `tyc profile` | Instrument emitted code for hot-function detection (advanced, opt-in). |
| `tyc migrate` | Convert typed Python (`.py`) to Typhon (`.ty`): rewrites `Optional[T]`/`T \| None` → `T?`, adds `let`/`mut` to module-level annotated assigns *and* function-body plain assignments, strips `@dataclass` decorators. |
| `tyc ty` | Build the project and run Astral's `ty` checker against the emitted Python. Requires `ty` on `PATH` (`pip install ty`). Supports `--watch` for continuous feedback. |
| `tyc stubtest` | Build the project and run `python -m mypy.stubtest` against every emitted `.pyi` stub. Complements `tyc check --stubs` (which performs an AST diff) by catching dynamically-created attributes the AST cannot see. Requires `mypy` in the chosen interpreter (`pip install mypy`). |
| `tyc repl` | Interactive Typhon evaluator. Reads `.ty` source one block at a time, compiles it through the full pipeline, and executes the result with a Python interpreter. |
| `tyc debug` | Build the project and launch the emitted Python under a debugger (default `pdb`). Thin v1 wrapper — a Typhon-native source-mapping debugger is a Phase-5 item. |
| `tyc run` | Execute a Typhon program. By default uses the in-process tree-walking VM ([docs/vm.md](vm.md)) — no `.py` is written, no CPython is spawned. `--compile` (alias `--no-vm`) falls back to the legacy build-then-exec path; pair with `--temp` to build into a tempdir that is removed on exit. |
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

# Emit Python (also generates pyproject.toml + .venv + runs `uv sync`)
tyc build
```

## `tyc build`

In addition to the codegen pipeline, every `tyc build` bootstraps the
Python environment for the project:

1. **`pyproject.toml` merge.** The keys this tool owns — `[project] name`,
   `version`, `requires-python`, `dependencies`, plus
   `[dependency-groups] dev` when `[dev-dependencies]` is non-empty —
   are derived from `typhon.toml` and written into `pyproject.toml` at
   the project root. If the file already exists, the merge is
   non-destructive: header comments, `[tool.*]` tables, and any
   `[project]` keys the user manages themselves (`authors`, `readme`,
   `classifiers`, …) are preserved byte-for-byte.
2. **`uv sync`.** Materialises `.venv` (creating it on first run) and
   installs the manifest. When `uv` isn't on `PATH`, or when `uv sync`
   itself returns non-zero, the failure is downgraded to a warning so
   the `.py` artefacts still land — the codegen output is useful
   regardless of whether the install step resolved.

The intent is that `tyc build` followed by `python build/main.py` works
out of the box on a freshly cloned project, no separate `tyc sync`
step required.

## `tyc migrate`

Converts typed Python (`.py`) to Typhon (`.ty`) in one pass:

- `Optional[T]` / `T | None` → `T?`
- Module-level annotated assignments (`x: int = 1`) gain `let` (or `mut` when later reassigned).
- Function-body plain assignments (`user = find_user(1)`, `total = 0`) gain `let` on first occurrence per function, promoted to `mut` if the same name is reassigned anywhere else in the file (the reassignment flag is file-wide, so an accumulator named `total` in one function will also tag a one-shot `total = 0` in another function as `mut` — a deliberate over-approximation, since `mut` on an unmutated binding still type-checks). Subsequent assignments to the same name in the same scope are left bare (correct re-binding). Class-body annotated assignments are left untouched — those are field declarations, not locals.
- `@dataclass` decorators and their `from dataclasses import dataclass` are dropped.

The output is designed to pass `tyc check` cleanly out of the box; accumulators / counters surfaced as `mut` are worth a manual review when porting larger codebases.

```bash
# Convert a single file (writes app.ty alongside app.py):
tyc migrate src/app.py

# Preview without writing (prints to stdout):
tyc migrate --check src/app.py
```

`--check` is a preview mode: it prints the migrated source to stdout instead of writing `.ty` files, but it does not compare against the input and always exits 0 on a successful migration. CI users who want a fail-on-diff signal should diff `--check` output against a checked-in `.ty`; a native exit-1-on-changes mode is a deliberate follow-up.

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

## `tyc stubtest`

Runtime probe that complements `tyc check --stubs`. The `--stubs` check performs an AST-level diff between every `.dty` stub and its sibling implementation; `tyc stubtest` adds the runtime introspection step that catches dynamically-created attributes the AST cannot see — `__init_subclass__` injection, metaclass-driven member registration, Pydantic auto-generated fields, and so on. Under the hood it builds the project and runs `python -m mypy.stubtest <module>` for every emitted `.pyi`.

```bash
# Build into a temp directory and probe every stub:
tyc stubtest

# Keep the build output for inspection:
tyc stubtest --out build/

# Re-use an existing build:
tyc stubtest --out build/ --no-build

# Probe against a specific virtualenv:
tyc stubtest --python .venv/bin/python

# Continue past the first failure to get the full drift report:
tyc stubtest --keep-going

# Forward extra flags to mypy.stubtest:
tyc stubtest -- --allowlist stubtest-allow.txt --ignore-positional-only
```

| Flag | Purpose |
|------|---------|
| `--out DIR` | Write emitted Python here instead of a temp dir |
| `--no-build` | Skip the build step; requires `--out` so the directory is known |
| `--python BIN` | Python interpreter (default: `python3`) |
| `--keep-going` | Probe every module even after one reports drift |

Requires `mypy` to be installed in the chosen interpreter (`pip install mypy`). When `mypy` is missing the command surfaces a clear error pointing the user at the install command rather than failing opaquely inside the subprocess.

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

## `tyc run`

Executes a Typhon program. Two execution modes:

- **VM (default).** Runs the source in the in-process tree-walking interpreter from `tyc-vm`. No `.py` is written, no CPython is spawned. See [docs/vm.md](vm.md) for the supported feature surface.
- **`--compile`** (alias `--no-vm`). Falls back to the legacy "build then exec CPython" path — required when your program imports CPython libraries the VM doesn't speak natively (`numpy`, `requests`, `pandas`, …).

The script's exit code propagates verbatim, so shell pipelines see the child's status unchanged. Parse, build, and spawn failures surface as the usual miette errors with exit code 1.

```bash
# Default (VM) — resolve src/main.ty under typhon.toml's [project] src:
tyc run

# Run a specific .ty file directly:
tyc run src/cli.ty

# Forward args to the script after `--` (populates sys.argv):
tyc run -- --port 8080 ./input.csv

# Fall back to compile-and-exec when the VM can't handle an import:
tyc run --compile

# Compile mode with an ephemeral build dir:
tyc run --compile --temp -- --port 8080

# Compile mode against a non-default entry point:
tyc run --compile --entry api.py
```

| Flag | Mode | Purpose |
|------|------|---------|
| `--compile` (alias `--no-vm`) | switch | Build to `.py` and exec CPython instead of using the VM |
| `--entry FILE` | compile | Entry-point file relative to the build dir (default `main.py`) |
| `--python PATH` | compile | Python interpreter (default `python3`) |
| `--temp` / `-t` | compile | Build into a tempdir that is deleted on exit; mutually exclusive with `--no-build` |
| `--no-build` | compile | Skip rebuilding; assume the persistent `build/` is current |

The compile-only flags (`--entry`, `--python`, `--temp`, `--no-build`) are rejected by clap unless `--compile` is also given, so a mistaken combination fails fast at the CLI rather than being silently ignored.

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

`tyc lsp` runs on stdio and speaks LSP. The reference VS Code extension wires it up; any LSP-aware editor can use it directly. Diagnostics, hover, go-to-definition, completion, and code actions are exposed through the same Salsa-backed query engine the CLI uses.

Completion has two modes. Open completion (cursor outside any member access) returns every binding visible from the cursor's scope plus the Typhon keyword and common-builtin sets. **Member-access completion** triggers on `.` and prefers **venv-driven introspection**: the LSP locates the project's `.venv/bin/python` (walking upward from the edited file for `typhon.toml`) and shells to it with a tiny embedded `dir(module) + inspect.signature` helper, so every stdlib module and every third-party package the user `uv add`-ed appears in the popup with real signatures and docstring first-lines. Each `(project, module)` result is cached for the session and invalidated when `.venv/pyvenv.cfg` mtime changes (covers `uv sync`, `uv pip install`, deleting `.venv`). Aliased imports (`import numpy as np`) resolve back to the source module before introspection. Dotted submodule access (`os.path.<TAB>`) is supported. Mid-keystroke buffers that don't parse — the typical state right after typing `.` — go through a single-character fix-up so the resolver can still see the surrounding imports. When no venv exists and no `python3` is on PATH, the LSP falls back to a small curated table of stdlib stubs (`os`, `sys`, `json`, `math`, `re`, `pathlib`, `datetime`, `collections`, `itertools`, `functools`, `typing`, `asyncio`, `logging`, `dataclasses`) so the editor remains useful before `uv sync`.
