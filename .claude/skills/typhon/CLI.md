# `tyc` Subcommand Reference

The full `tyc` surface. For background and design rationale, see `docs/cli.md` and `docs/long-term-plan.md`.

`tyc` is a single Rust binary built from `tyc/Cargo.toml`. Each subcommand reuses the same Salsa-backed pipeline (`tyc-syntax → tyc-resolve → tyc-types → tyc-analyse → tyc-desugar → tyc-emit → tyc-format`).

```bash
# One-time: build the compiler
cd tyc && cargo build --release && cd ..
alias tyc="$PWD/tyc/target/release/tyc"
```

---

## Daily commands

### `tyc check [PATHS]`

Parse, resolve, type-check, and analyse — **no emit**. The CI-recommended command.

```bash
tyc check src/
tyc check src/ tests/
tyc check --stubs            # also diff every .dty against the module it describes
```

Exits non-zero on any `tyc::*` diagnostic at `"error"` severity. Diagnostics are formatted via miette.

### `tyc build [PATHS]`

Full pipeline: parse → check → analyse → desugar → emit → format. Writes:

- `build/*.py` — the emitted modules.
- `build/*.py.map` — v2 source maps (per-statement `out_line → ty_line`).
- `build/typhon_runtime.py` — generated if you use `Result`, `go`, or `lazy let`.
- `build/*.pyi` — companion stubs for every `.dty`.

`[emit] format = true` (the default) runs `ruff format` post-emit.

### `tyc fmt [PATHS]`

Parse and pretty-print `.ty` source in place. Wraps `ruff format` applied to a Typhon-aware printer. Idempotent.

```bash
tyc fmt src/
tyc fmt src/main.ty
```

### `tyc lsp`

Run on stdio as a Language Server. Features today (verify against current code if it matters — the LSP grows):

- Diagnostics on `did_open` / `did_change`.
- Position-based hover (binding kind + mutability).
- Go-to-definition (cross-file, `.ty` ↔ `.py` aware via `.py.map`).
- Completions (visible bindings + Typhon keywords + common builtins).
- Attribute resolution against class definitions and re-export-aware import resolution.
- "Remove unused import" code action.

Editor wiring: `editors/vscode/` ships a reference extension. Any LSP-aware editor can attach `tyc lsp` directly.

### `tyc init NAME`

Scaffold a new project:

```
NAME/
├── typhon.toml      (defaults; see configuration.md)
├── src/
│   └── main.ty      (the canonical "Hello, world")
└── tests/
```

The default `typhon.toml` enables `[strictness] no-implicit-any = true`, `unused-import = "error"`, `exhaustive-match = "error"`, and `[emit] format = true`.

---

## Debugging emitted code

### `tyc trace TRACEBACK_FILE`

Read a captured Python traceback and rewrite frames back to `.ty` source via the `.py.map` sidecars.

```bash
python build/main.py 2> err.log
tyc trace err.log
```

Use after a production / CI failure where you only have the traceback. Pair with `tyc debug` for live stepping.

### `tyc profile`

Builds, then instruments every top-level function with call-count + wall-clock sampling. Writes `typhon-profile.json` on interpreter exit. Feeds `[strictness] pgo-memoise`:

```bash
tyc profile -- some-realistic-workload
# → typhon-profile.json
# Now enable [strictness] pgo-memoise = true and `tyc build` will
# promote hot pure functions to @functools.cache.
```

### `tyc debug`

Builds, then execs `python -m pdb build/main.py` (default debugger). v1 wrapper — a Typhon-native source-mapping debugger is a Phase-5 item.

```bash
tyc debug
tyc debug --entry api.py --debugger pudb
tyc debug -- --verbose --port 8080      # forward args to the script
```

Flags:

| Flag | Default | Purpose |
|---|---|---|
| `--entry FILE` | `main.py` | Entry-point relative to the build dir |
| `--python PATH` | `python3` | Interpreter |
| `--debugger MODULE` | `pdb` | Module to launch under `python -m` |
| `--no-build` | — | Skip rebuild; assume `build/` is current |

Frames surface as `build/*.py` paths; pair with `tyc trace` to remap captured tracebacks.

---

## Migration

### `tyc migrate [--check] PATH`

Convert typed Python (`.py`) → Typhon (`.ty`) in one pass:

- `Optional[T]` / `T | None` → `T?` in annotations.
- Module-level annotated assignments (`x: int = 1`) gain `let` (or `mut` if later reassigned).
- `@dataclass` decorators and `from dataclasses import dataclass` are dropped (Typhon `class` emits dataclasses).

```bash
tyc migrate src/app.py            # writes src/app.ty alongside src/app.py
tyc migrate --check src/app.py    # preview to stdout, exit 0 if already Typhon-compatible
```

`--check` is useful in CI to confirm a `.py` file is migration-ready. After migration, run `tyc check src/` and fix the remaining diagnostics manually — the migration can't infer `let`/`mut` inside function bodies (everything starts as `let`; mark accumulators / counters as `mut`).

---

## Second-opinion type-checking

### `tyc ty`

Builds the project, then runs [Astral's `ty`](https://github.com/astral-sh/ty) checker against the emitted Python. Independent verification that the lowering is sound.

```bash
pip install ty                    # or: uv tool install ty
tyc ty
tyc ty --out build/               # write emitted Python to an explicit directory
tyc ty --watch                    # re-run on .ty / .dty changes
tyc ty -- --strict                # forward flags to `ty check`
```

| Flag | Purpose |
|---|---|
| `--out DIR` | Write emitted Python here instead of a temp dir |
| `--ty-bin BIN` | Path to the `ty` executable (default: `ty`) |
| `--no-build` | Skip the build step; requires `--out` |
| `--watch` | Watch source dir and re-run on `.ty` / `.dty` changes |

See `docs/ty-integration.md` for the integration design notes.

---

## REPL

### `tyc repl`

Interactive Typhon evaluator. Each prompt accumulates source, recompiles the whole session through the full Typhon pipeline, and executes the result with a Python interpreter. Only the new tail of stdout is displayed after each input.

```bash
tyc repl                          # auto-detects python3.13 / python3.12 / python3
tyc repl --load src/lib.ty       # pre-load a .ty file as the initial session
tyc repl --python python3.13     # specific interpreter
```

REPL meta-commands:

| Input | Effect |
|---|---|
| `:quit` | Exit |
| `:reset` | Clear accumulated session |
| `:show` | Dump current session source |

Known limitations:

- Each prompt **re-executes the entire accumulated session**. Side effects fire once per prompt.
- Multi-line blocks end on the first blank line.
- No readline / arrow-key support yet.

For exploratory typing, paired with `tyc check src/` in a watcher, is usually a better dev loop.

---

## Package management (`uv` surface)

### `tyc add PACKAGE[@VERSION]`

Rewrite `[dependencies]` / `[dev-dependencies]` in `typhon.toml`, then shell out to `uv` for the install.

```bash
tyc add requests                  # bare name → "*" (any version)
tyc add requests@2.31             # → ">=2.31" (semver-prefix conversion)
tyc add --dev pytest@8.2          # under [dev-dependencies]
tyc add --no-sync foo bar baz     # batch edits; finish with `tyc sync`
```

If `uv` is missing, `tyc add` still rewrites the manifest and prints an "install uv" message.

### `tyc remove PACKAGE`

```bash
tyc remove rich
tyc remove --dev pytest
tyc remove --no-sync foo          # don't run uv afterwards
```

### `tyc sync`

Materialise `[dependencies]` into a generated `pyproject.toml` and run `uv sync` to install.

```bash
tyc sync
tyc sync --dry-run                # print the generated pyproject.toml, don't write/install
```

---

## CI integration

Recommended pipeline:

```yaml
- run: tyc check src/                  # primary gate; non-zero on any error severity
- run: tyc check --stubs               # if you ship .dty stubs
- run: tyc ty                          # second-opinion (needs `pip install ty`)
```

For projects that publish their `tyc`-built `.py` alongside Typhon source:

```yaml
- run: tyc build
- run: tyc trace ci-traceback.log      # optional; useful when integration tests fail
```

---

## Exit codes (uniform across subcommands)

| Code | Meaning |
|---|---|
| `0` | Success, no diagnostics at `"error"` severity |
| `1` | Diagnostics at `"error"` severity present |
| `2` | Usage / argument error |
| `>2` | Unexpected internal error (please file an issue with the full output) |

Diagnostics at `"warn"` severity do **not** affect exit code.
