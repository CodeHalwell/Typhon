# `tyc` Subcommand Reference

The full `tyc` surface. For background and design rationale, see `docs/cli.md` and `docs/long-term-plan.md`.

`tyc` is a single Rust binary built from `tyc/Cargo.toml`. Each subcommand reuses the same Salsa-backed pipeline (`tyc-syntax → tyc-resolve → tyc-types → tyc-analyse → tyc-desugar → tyc-emit → tyc-format`); third-party introspection rides on the side crate `tyc-venv`. The current release is **v0.15.6**.

```bash
# One-time: build the compiler
cd tyc && cargo build --release && cd ..
alias tyc="$PWD/tyc/target/release/tyc"

# Or install a pre-built binary
curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh   # macOS/Linux
iwr -useb https://raw.githubusercontent.com/codehalwell/typhon/main/install.ps1 | iex  # Windows PowerShell
```

---

## Daily commands

### `tyc check [PATHS]`

Parse, resolve, type-check, and analyse — **no emit**. The CI-recommended command.

```bash
tyc check src/
tyc check src/ tests/
tyc check --stubs            # also diff every .dty against the module it describes
tyc check --with-ty          # (v0.12.0) also run Astral's `ty` over a throwaway build
```

Exits non-zero on any `tyc::*` diagnostic at `"error"` severity. Diagnostics are formatted via miette.

**v0.3.1: grouped diagnostics by source file** — errors render as `-- errors in ./src/a.ty --` blocks instead of interleaved by analysis phase, with a per-code summary tally and `tyc explain <code>` suggestion at the bottom.

**v0.12.0: third-party argument-type checking.** `tyc check` now consults venv signature introspection (the `tyc-venv` crate) for the *types* of arguments to fully-typed third-party functions / constructors, not just their arity — a wrong-typed argument fires `tyc::type_mismatch`. A declared dependency that's imported but can't be introspected surfaces the `unintrospectable-dependency` warning (`[strictness] unintrospectable-dependency`, default `"warn"`). `--with-ty` (normally emit-free `check` builds to a throwaway directory first) additionally runs the `ty` typeshed pass — see [`tyc ty`](#tyc-ty) and `[checker]` below.

### `tyc build [PATHS]`

Full pipeline: parse → check → analyse → desugar → emit → format. Writes:

- `build/*.py` — the emitted modules.
- `build/.sourcemaps/*.py.map` — v2 source maps (per-statement `out_line → ty_line`). Location moved here in v0.6.1; legacy adjacent layout still readable.
- `build/typhon_runtime/` — generated package if you use `Result`, `go`, `lazy let`, `freeze let`, or auto-parallel.
- `build/*.pyi` — companion stubs for every `.dty`.
- `build/pyproject.toml` — merged with user-managed `[tool.*]` tables.

`[emit] format = true` (the default) runs `ruff format` post-emit.

| Flag | Effect |
|---|---|
| `--out DIR` / `-o DIR` | Override `[project] out` |
| `--no-format` | Skip `ruff format` post-process |
| `--check` | Dry-run — list every file that *would* be created/overwritten without touching disk. Full pipeline still runs, so type errors surface |
| `--no-sync` (env: `TYC_NO_SYNC=1`) | Skip `uv sync` but still merge `pyproject.toml` |
| `--with-ty` (v0.12.0) | After emit, run Astral's `ty` over the emitted Python (typeshed-backed second-stage check); errors fail the build. Equivalent to `[checker] external = "ty"` for one invocation |

When `[checker] external = "ty"` is set in `typhon.toml` (or `--with-ty` is passed), `tyc build` runs the `ty` pass automatically after a successful emit, re-attributing `ty`'s diagnostics to the originating `.ty` line via the `.py.map` sidecars. This is the only path that type-checks against **typeshed** (C-extension + stdlib APIs that venv introspection can't model). Requires `ty` on `PATH`. `[checker] external-args = [...]` forwards extra flags verbatim.

### `tyc fmt [PATHS]`

Parse and pretty-print `.ty` source in place. Wraps `ruff format` applied to a Typhon-aware printer. Idempotent.

```bash
tyc fmt src/
tyc fmt src/main.ty
```

In-process whitespace pass handles: trailing spaces, final newline, leading-tab expansion, line-ending normalisation, and **five PEP 8 spacing rules** (v0.3.0): space after `:`, spaces around `->`, space after `,`, single-space around binary `+`/`-` and top-level `=`, two blank lines before top-level `def`/`class`/`async def`. Falls back silently to in-process output on `ruff` failure.

Scientific-notation literals (`1e-12`, `2.5E+7`, `1.0e-12`) survive the whitespace pass intact (v0.7.0).

### `tyc lsp`

Run on stdio as a Language Server. Features today (verify against current code if it matters — the LSP grows):

- Diagnostics on `did_open` / `did_change`.
- Position-based hover (binding kind + mutability + signature; surfaces venv introspection failure reasons in v0.3.1).
- Go-to-definition (cross-file, `.ty` ↔ `.py` aware via `.py.map`).
- Completions (visible bindings + Typhon keywords + common builtins).
- Attribute resolution against class definitions and re-export-aware import resolution.
- Venv-driven member-access introspection (cached per session, invalidated on `pyvenv.cfg` mtime change; per-module timeout 10s as of v0.3.1).
- Prewarm dotted module path (v0.3.1): `import torch.nn as nn` warms `torch.nn`, not just `torch`.
- Semantic tokens: `newtype` paints as `class` at declaration and references (v0.6.0); class-body fields paint as `property` (v0.6.0).
- "Remove unused import" code action.

Editor wiring: `editors/vscode/` ships a reference extension (v0.2.0 as of v0.9.0). Any LSP-aware editor can attach `tyc lsp` directly.

### `tyc init NAME`

Scaffold a new project:

```
NAME/
├── typhon.toml      # every [strictness]/[emit] key commented
├── src/
│   └── main.ty      # canonical "Hello, world" — frozen dataclass + impl + Result/?/match
└── tests/
```

The default `typhon.toml` enables `[strictness] no-implicit-any = true`, `unused-import = "error"`, `exhaustive-match = "error"`, and `[emit] format = true`.

---

## Run / execute

### `tyc run [PATHS]`

Execute a Typhon program. **Default mode: the in-process tree-walking VM (`tyc-vm`)** — no `.py` written, no CPython spawn.

```bash
tyc run                              # VM, resolves src/main.ty
tyc run src/cli.ty                   # VM on a specific file
tyc run -- --port 8080 ./input.csv   # forward args to sys.argv
tyc run --compile                    # build-then-exec via CPython
tyc run --compile --temp -- --port 8080
```

| Flag | Mode | Purpose |
|---|---|---|
| `--compile` (alias `--no-vm`) | switch | Build → exec CPython instead of using the VM. Use for programs that import unsupported libraries (numpy, requests, etc.) |
| `--entry FILE` | compile-only | Entry-point relative to build dir (default `main.py`) |
| `--python PATH` | compile-only | Python interpreter (default `python3`) |
| `--temp` / `-t` | compile-only | Build into a tempdir deleted on exit; mutually exclusive with `--no-build` |
| `--no-build` | compile-only | Skip rebuild; assume persistent `build/` is current |

Compile-only flags are rejected by clap unless `--compile` is also passed. The script's exit code propagates verbatim.

**v0.3.1:** `tyc run` (VM mode) gates on the static `tyc check` pipeline first — unresolved names, type mismatches, and arity errors fail the same way `tyc check` would. Set `TYC_SKIP_CHECK=1` to bypass. `--compile` mode always gates on the full `tyc build` pipeline (no equivalent bypass).

**v0.8.0:** `tyc run --compile` rejects single-file inputs up-front with an actionable error pointing at the project-style layout (compile mode requires `typhon.toml`-driven multi-file mode because the emitter writes `build/main.py` + `build/typhon_runtime/...` alongside).

**v0.9.0:** the VM now handles multi-file projects natively. Sibling `.ty` modules under the project source root load on demand; relative imports (`from .repo import x`, `from ..pkg.users import load`) resolve through a `Value::Module` cache. `tyc run --compile` now spawns `python -m <pkg>.main` instead of `python build/main.py` so relative imports in the entry point resolve correctly under the compiled path too. The VM also gained `Result` combinators, write/append/binary `open()` modes, class patterns on built-in types, `frozenset` dict keys, deep `freeze let`, comptime substitution, `lazy import np = numpy`, `class!` exception fields, `dataclasses.field(default_factory=...)` per-instance factories, and native shims for `collections.deque` / `heapq` / `contextlib` / `pydantic`. See [RUNTIME.md](RUNTIME.md) §2.4–§2.4c for the full surface.

See [RUNTIME.md](RUNTIME.md) for the VM's full feature surface and the fallback rules.

---

## Debugging emitted code

### `tyc trace TRACEBACK_FILE`

Read a captured Python traceback and rewrite frames back to `.ty` source via the `.py.map` sidecars.

```bash
python build/main.py 2> err.log
tyc trace err.log
```

Use after a production / CI failure where you only have the traceback. Pair with `tyc debug` for live stepping. Paths with spaces use a longest-candidate walk-left lookup (v0.5.0). Cached per-line.

### `tyc profile`

Builds, then instruments every top-level function with call-count + wall-clock sampling. Writes `typhon-profile.json` on interpreter exit. Feeds `[strictness] pgo-memoise`:

```bash
tyc profile -- some-realistic-workload
# → typhon-profile.json
# Now enable [strictness] pgo-memoise = true and `tyc build` will
# promote hot pure functions to @functools.cache.
```

### `tyc debug`

Builds, then execs `python -m pdb build/main.py` (default debugger).

**v0.5.0: Typhon-aware pdb wrapper.** `tyc debug` writes a one-shot Python wrapper that subclasses `pdb.Pdb`, loads every `.py.map` under the build directory at startup, and overrides `do_list`, `do_where`, `format_stack_entry`, and `prompt` so the **entire debugger UI reads `.ty` coordinates** — list output, stack traces, the prompt itself. Pass `--raw-pdb` to opt out.

```bash
tyc debug
tyc debug --entry api.py --debugger pudb
tyc debug --break src/main.ty:42                  # Typhon-coordinate breakpoint
tyc debug --break src/main.ty:42 --break src/handlers.ty:100   # repeatable
tyc debug --raw-pdb                                 # skip the source-mapping wrapper
tyc debug -- --verbose --port 8080                  # forward args to the script
```

| Flag | Default | Purpose |
|---|---|---|
| `--entry FILE` | `main.py` | Entry-point relative to the build dir |
| `--python PATH` | `python3` | Interpreter |
| `--debugger MODULE` | `pdb` | Module to launch under `python -m` (e.g. `pudb`, `ipdb`, `debugpy`) |
| `--break TY:LINE` | — | Set breakpoint at `<ty-file>:<line>` — translated through `.py.map`. Repeatable. Windows-style `C:\foo.ty:10` and POSIX paths both work |
| `--no-build` | — | Skip rebuild; assume `build/` is current |
| `--raw-pdb` | — | Opt out of the source-mapping wrapper |

Frames surface as `[ty] <src>:<line>` after every pause. Pair with `tyc trace` for captured-traceback remapping.

---

## Migration

### `tyc migrate [--check] PATH`

Convert typed Python (`.py`) → Typhon (`.ty`) in one pass:

- `Optional[T]` / `T | None` / `Union[T, None]` (incl. `typing.Union[...]` qualified form) → `T?` in annotations. Multi-arm `Union` falls through to PEP 604 pipe syntax. Dead `typing.Union` import elided.
- `Optional["Item"]` → `"Item?"` (the `?` goes *inside* the forward-ref string; v0.3.0).
- `class X(Generic[T]):` → `class X[T]:` (PEP 695). Multi-parameter, mixed bases, qualified `typing.Generic` forms all covered (v0.3.1). Module-level `T = TypeVar("T")` (incl. bounded / `constraints=`) is **dropped**. `TypeVar` / `Generic` imports elided when no longer referenced.
- `NewType("X", T)` → `newtype X = T` (v0.5.0). Matching `from typing import NewType` entries pruned.
- `class X(Protocol):` → `interface X:` (v0.5.0). `class X(Protocol[T]):` → `interface X[T]:`. Multi-base forms untouched.
- `@dataclass(frozen=True[, ...])` → `class X frozen:` (v0.5.0). The `@dataclass` decorator and `from dataclasses import dataclass` are dropped (Typhon `class` emits dataclasses).
- Module-level annotated assignments (`x: int = 1`) gain `let` (or `mut` if later reassigned).
- Function-body plain assignments gain `let` on first occurrence per function, promoted to `mut` if the same name is reassigned anywhere else in the file (file-wide flag — deliberate over-approximation; `mut` on an unmutated binding still type-checks).
- Class-body annotated assignments left untouched (those are field declarations).

```bash
tyc migrate src/app.py            # writes src/app.ty alongside src/app.py
tyc migrate --check src/app.py    # preview to stdout, exit 0 if already Typhon-compatible
```

`--check` is useful in CI to confirm a `.py` file is migration-ready. After migration, run `tyc check src/` and fix the remaining diagnostics manually — the migration can't infer `let`/`mut` inside function bodies (everything starts as `let`; mark accumulators / counters as `mut`).

The third-party-corpus sweep at `stress/third-party-py-corpus/` round-trips representative fixtures through `tyc migrate` + `tyc check` in CI; the opt-in nightly `stress/pypi-sweep/sweep.py` pip-installs `attrs` / `click` / a small Pydantic-using package and round-trips them.

---

## Second-opinion type-checking

### `tyc ty`

Builds the project, then runs [Astral's `ty`](https://github.com/astral-sh/ty) checker against the emitted Python. Independent verification that the lowering is sound.

**v0.5.0: diagnostic attribution via `.py.map`.** Captures `ty`'s output and rewrites every `path.py:LINE[:COL]:` reference back to `.ty` coordinates via the source-map loader (`commands/source_map.rs`). Pass `--raw` to forward output verbatim.

```bash
pip install ty                    # or: uv tool install ty
tyc ty
tyc ty --out build/               # write emitted Python to an explicit directory
tyc ty --watch                    # re-runs on .ty/.dty change
tyc ty -- --strict                # forward flags to `ty check`
tyc ty --raw                      # disable .ty path attribution
```

| Flag | Purpose |
|---|---|
| `--out DIR` | Write emitted Python here instead of a temp dir |
| `--ty-bin BIN` | Path to the `ty` executable (default: `ty`) |
| `--no-build` | Skip the build step; requires `--out` |
| `--watch` | Watch source dir and re-run on `.ty` / `.dty` changes |
| `--raw` | Opt out of `.py:LINE:COL` → `.ty:LINE:COL` rewriting |

**v0.12.0: the same `ty` pass is wired into the build.** Set `[checker] external = "ty"` in `typhon.toml` (or pass `--with-ty` to `tyc build` / `tyc check`) to run `ty` automatically after a successful emit — `ty` errors fail the build, so it's CI-gating. The standalone `tyc ty` command and the build hook share one `run_ty_check` helper. `tyc ty` remains the way to run it ad-hoc (with `--watch`, explicit `--out`, etc.). The embedded in-process `ty` (Phase 2) was prototyped but is **not shipped** — it needs a git dependency the repo's `cargo deny` policy disallows, and adds no capability over the subprocess path.

See `docs/ty-integration.md` for the integration design notes.

### `tyc stubtest`

Builds, then runs `python -m mypy.stubtest <module>` against every emitted `.pyi`. Runtime introspection probe complementing `tyc check --stubs`'s AST diff — catches dynamically-created members the AST can't see (`__init_subclass__` injection, metaclass-driven member registration, Pydantic auto-generated fields).

```bash
tyc stubtest
tyc stubtest --keep-going          # probe every module past first failure
```

| Flag | Purpose |
|---|---|
| `--out DIR` | Write emitted Python here instead of tempdir |
| `--no-build` | Skip build (requires `--out`) |
| `--python BIN` | Default `python3` |
| `--keep-going` | Probe every module past first failure |

Requires `mypy` in the chosen interpreter. Missing `mypy` surfaces a clear install-pointer error.

---

## Diagnostics catalog

### `tyc explain CODE`

Prints the catalog entry for a `tyc::` code (mirrors `rustc --explain`). Accepts short (`immutable_assign`) or fully-qualified (`tyc::immutable_assign`).

```bash
tyc explain immutable_assign
tyc explain tyc::nullable_use
tyc explain --list                # print every code one per line
tyc explain --list | fzf | xargs tyc explain   # interactive picker
```

Every diagnostic carries a `url(https://typhon.dev/lang/diagnostics/<code>)` miette attribute; catalog pages also live under `docs/diagnostics/<code>.md`.

### `tyc cheatsheet`

Prints `docs/cheatsheet.md` to stdout. Embedded in the binary; works offline.

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
- Prompts are skipped when stdin is piped (v0.3.0 O26 fix).

Bare single-line expressions auto-print their `repr(...)` — `>>> 1 + 1` prints `2` — matching the universal REPL convention.

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

The generated `pyproject.toml` carries `[project] name/version/requires-python/dependencies` plus `[dependency-groups].dev` when `[dev-dependencies]` is non-empty; user-managed `[tool.*]` tables and other `[project]` keys are preserved byte-for-byte.

---

## Tooling

### `tyc install`

Materialise embedded tooling assets into the current project. The one target today is the `typhon` Claude skill.

```bash
tyc install skill                 # write .claude/skills/typhon/ into the current project
tyc install skill --force         # overwrite an existing copy
tyc install skill --dir ../other  # target a different project root
tyc install skill --list          # print the files that would be written, write nothing
```

`tyc install skill` writes the whole skill tree — `SKILL.md`, every sibling reference (`REFERENCE.md`, `CLI.md`, `PITFALLS.md`, `DIAGNOSTICS.md`, `COOKBOOK.md`, `RUNTIME.md`, `PACKAGING.md`), and the `references/` examples folder with its index — into `<dir>/.claude/skills/typhon/`. The skill is **embedded in the `tyc` binary at build time** (`include_str!`), so the command works from any directory with no network access and no dependency on the Typhon source checkout.

| Flag | Effect |
|---|---|
| `--force` | Overwrite files that already exist. Without it, `tyc install skill` refuses if `.claude/skills/typhon/SKILL.md` is already present (exit code `1`) so an existing, possibly-customised copy is never clobbered |
| `--dir PATH` | Install into `PATH/.claude/skills/typhon/` instead of the current directory |
| `--list` | Dry-run: print every relative path that would be written, then exit `0` without touching disk |

The installed copy is a verbatim snapshot of the skill shipped with the `tyc` you ran, so `tyc --version` tells you which release of the skill you'll get. Re-run with `--force` after upgrading `tyc` to refresh a vendored copy.

---

## CI integration

Recommended pipeline:

```yaml
- run: tyc check src/                  # primary gate; non-zero on any error severity
- run: tyc check --stubs               # if you ship .dty stubs
- run: tyc ty                          # second-opinion (needs `pip install ty`)
- run: tyc stubtest                    # runtime probe (needs mypy in the venv)
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
| `1` | Diagnostics at `"error"` severity present (or parse/type/build/spawn failure) |
| `2` | Usage / argument error |
| `>2` | Unexpected internal error (please file an issue with the full output) |

Diagnostics at `"warn"` severity do **not** affect exit code.

For `tyc run` / `tyc debug`, the child's exit code propagates verbatim.

---

## Environment variables

| Var | Effect |
|---|---|
| `TYPHON_VERSION` | Used by `install.sh` / `install.ps1` to pin a release tag (default: latest) |
| `TYPHON_INSTALL_DIR` | Used by installers to override the install location |
| `TYC_SKIP_CHECK=1` | Skip the pre-VM static check in `tyc run` (v0.3.1) — does NOT affect `--compile` mode |
| `TYC_NO_SYNC=1` | Equivalent to `--no-sync` on `tyc build` and `tyc add`/`tyc remove` |
