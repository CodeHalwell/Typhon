# CLI Reference

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Typhon ships a single binary, `tyc`, that handles every stage of the workflow. Subcommands are built with `clap` v4.

## Subcommands

| Command | Purpose |
|---------|---------|
| `tyc build` | Full pipeline: parse, check, analyse, desugar, emit, format. Also bootstraps the Python environment — merges the owned keys (`[project] name/version/requires-python/dependencies`, plus `[dependency-groups].dev` when `[dev-dependencies]` is non-empty) into `pyproject.toml` (preserving any user-managed `[tool.*]` / other `[project]` keys) and runs `uv sync` so `.venv` is ready. `uv sync` failure is downgraded to a warning. `--check` is a dry-run mode that lists every file that *would* be written without touching disk. |
| `tyc check` | Up to analyser, no emit. Used by CI. |
| `tyc fmt` | Format `.ty` source. The pipeline runs a Typhon-aware whitespace normaliser first (collapsing interior whitespace, tidying bracket/comma spacing, expanding leading tabs, normalising line-endings) and then, when the post-normalised buffer contains no Typhon-only tokens, pipes it through `ruff format` for the spacing rules around `:`, `=`, and `->`. If `ruff` is missing on `PATH` or fails, the in-process output is kept. |
| `tyc lsp` | Run as a Language Server. |
| `tyc init` | Scaffold a new project: `typhon.toml`, `src/`, `tests/`. The generated `src/main.ty` includes a frozen dataclass, an `impl` block, a `mut` binding, and a `Result`/`?`/`match` example; the generated `typhon.toml` ships every `[strictness]` / `[emit]` key with a comment. |
| `tyc install skill` | Write the embedded `typhon` Claude skill (`SKILL.md` + sibling reference docs + the `references/` example programs) into `.claude/skills/typhon/` of the current project. The skill is bundled into the binary at build time, so it works offline. `--force` overwrites an existing copy, `--dir PATH` targets another root, `--list` previews the files without writing. |
| `tyc trace` | Map a Python traceback back to Typhon source via `.py.map` files. |
| `tyc profile` | Instrument emitted code for hot-function detection (advanced, opt-in). |
| `tyc migrate` | Convert typed Python (`.py`) to Typhon (`.ty`): rewrites `Optional[T]`/`T \| None` → `T?`, adds `let`/`mut` to module-level annotated assigns *and* function-body plain assignments, strips `@dataclass` decorators. |
| `tyc ty` | Build the project and run Astral's `ty` checker against the emitted Python. Requires `ty` on `PATH` (`pip install ty`). Supports `--watch` for continuous feedback. |
| `tyc stubtest` | Build the project and run `python -m mypy.stubtest` against every emitted `.pyi` stub. Complements `tyc check --stubs` (which performs an AST diff) by catching dynamically-created attributes the AST cannot see. Requires `mypy` in the chosen interpreter (`pip install mypy`). |
| `tyc repl` | Interactive Typhon evaluator. Reads `.ty` source one block at a time, compiles it through the full pipeline, and executes the result with a Python interpreter. |
| `tyc debug` | Build the project and launch the emitted Python under a debugger (default `pdb`). Repeatable `--break <ty-file>:<line>` flags translate Typhon source locations through the v2 `.py.map` and inject `-c "break …"` into the debugger session, so breakpoints set on `.ty` lines fire on the corresponding emitted Python lines. |
| `tyc run` | Execute a Typhon program. By default uses the in-process tree-walking VM ([docs/vm.md](vm.md)) — no `.py` is written, no CPython is spawned. `--compile` (alias `--no-vm`) falls back to the legacy build-then-exec path; pair with `--temp` to build into a tempdir that is removed on exit. |
| `tyc explain <code>` | Print the diagnostic catalog entry for a `tyc::` code (mirrors `rustc --explain`). Accepts the short form (`immutable_assign`) or the fully-qualified `tyc::immutable_assign`. Use `tyc explain --list` to print every code the explainer knows about. Every catalog page also lives at `docs/diagnostics/<code>.md` and is linked from the `url(...)` clause on each diagnostic. |
| `tyc cheatsheet` | Print the 30-second Typhon cheat sheet (from [docs/cheatsheet.md](cheatsheet.md)) to stdout. Handy when you need a syntax refresher without leaving the terminal. |
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
3. **`.py` files in `src/` are copied verbatim** to the output dir
   (skipping `__pycache__/`, `tests/`, `.venv/`, and dotfiles), so a
   hand-written Python helper next to your `.ty` source is importable
   from the emitted code. A relative `.py` import that resolves outside
   `src/` fires `tyc::orphan_py_import`.

The intent is that `tyc build` followed by `python build/main.py` works
out of the box on a freshly cloned project, no separate `tyc sync`
step required.

| Flag | Effect |
|------|--------|
| `--out DIR` / `-o` | Override the `[project] out` directory. Relative paths resolve against the project root. |
| `--no-format` | Skip the `ruff format` post-process. |
| `--check` | Dry-run: list every file that *would* be created or overwritten without touching disk. The full pipeline still runs, so type errors continue to surface. |
| `--with-ty` (v0.12.0) | After a successful emit, run Astral's `ty` over the emitted Python (typeshed-backed second-stage check) and re-attribute its diagnostics to the `.ty` source. `ty` errors fail the build. Equivalent to setting `[checker] external = "ty"` for one invocation; requires `ty` on `PATH`. |

When `[checker] external = "ty"` is set in `typhon.toml` (see [configuration.md](configuration.md#checker)), the same `ty` pass runs automatically after every `tyc build`. `tyc check --with-ty` works too — since `check` is normally emit-free, it builds to a throwaway directory first. This is the only path that type-checks against **typeshed**, so it catches misuse of C-extension and stdlib APIs that runtime venv introspection can't model (e.g. `os.path.join(1, 2)`). See [`tyc ty`](#tyc-ty) and [ty-integration.md](ty-integration.md).

> **Third-party argument-type checking (v0.12.0).** Independently of `ty`, `tyc build` / `tyc check` now type-check the *arguments* to fully-typed third-party functions and constructors via venv signature introspection (not just their arity) — a wrong-typed argument fires `tyc::type_mismatch`. A declared dependency that's imported but can't be introspected surfaces the `unintrospectable-dependency` warning (`[strictness] unintrospectable-dependency`, default `"warn"`).

## `tyc explain`

Print the catalog entry for any `tyc::` diagnostic. Useful when a
diagnostic in the terminal isn't self-explanatory and you don't want to
context-switch to a browser.

```bash
tyc explain immutable_assign           # short code
tyc explain tyc::immutable_assign      # fully-qualified — both work
tyc explain --list                     # print every code the explainer knows about, one per line
```

The `--list` flag prints one fully-qualified code per line, suitable for piping
to `grep` or `fzf` (`tyc explain --list | fzf | xargs tyc explain`).

Every diagnostic emitted by `tyc` also carries a `url(https://typhon.dev/lang/diagnostics/<code>)`
attribute (rendered inline by miette), so the same page is one click away
in any terminal that linkifies URLs. The full catalog lives under
[`docs/diagnostics/`](diagnostics/README.md).

## `tyc cheatsheet`

Print the 30-second Typhon cheat sheet ([`docs/cheatsheet.md`](cheatsheet.md))
to stdout. Same content the `tyc init` scaffold links to, kept embedded
in the binary so it works offline.

```bash
tyc cheatsheet                  # straight to stdout
tyc cheatsheet | less           # paginate it
```

## `tyc migrate`

Converts typed Python (`.py`) to Typhon (`.ty`) in one pass:

- `Optional[T]` / `T | None` → `T?`
- `Union[T, None]` (and `Union[None, T]`, including the `typing.Union[...]` qualified form) → `T?`; multi-arm unions fall through to PEP 604 pipe syntax, and the now-dead `typing.Union` import is elided.
- `class X(Generic[T]):` → `class X[T]:` (PEP 695). Multi-parameter generics (`Generic[T, U]` → `[T, U]`), mixed bases (`class X(Generic[T], OtherBase):` → `class X[T](OtherBase):`), and qualified `typing.Generic` forms are all covered. Module-level `T = TypeVar("T")` declarations (including bounded / `constraints=` forms) are dropped because Typhon's PEP 695 syntax expresses bounds inline; the `TypeVar` / `Generic` imports are elided when no longer referenced.
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
| `--raw` | Forward `ty`'s output verbatim (opt out of `.py` → `.ty` source-map re-attribution) |

**Build-integrated since v0.12.0.** The same `ty` pass is wired into the build via `[checker] external = "ty"` (or `--with-ty` on `tyc build` / `tyc check`) — see [`tyc build`](#tyc-build). The standalone `tyc ty` command and the build hook share one `run_ty_check` helper, so `tyc ty` stays the way to run it ad-hoc (watch mode, explicit output dir, extra flags). An embedded in-process `ty` checker (Phase 2) was prototyped and proven feasible but is **not shipped**: it requires a git dependency on `astral-sh/ruff` that the repo's `cargo deny` policy disallows, and offers no capability the subprocess path lacks. See [ty-integration.md](ty-integration.md).

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

Builds the project, then execs the configured Python debugger on the emitted entry-point. Frames surface as `build/*.py` paths; pair with `tyc trace` to remap captured tracebacks back to `.ty` source via the `.py.map` sidecars.

```bash
# Step through build/main.py under pdb:
tyc debug

# Different entry point + debugger:
tyc debug --entry api.py --debugger pudb

# Forward args to the script:
tyc debug -- --verbose --port 8080

# Set a breakpoint on a Typhon line. `tyc` translates it through .py.map
# and passes `-c "break build/main.py:N"` to the debugger:
tyc debug --break src/main.ty:42

# Multiple breakpoints stack:
tyc debug --break src/main.ty:42 --break src/lib/io.ty:7
```

| Flag | Purpose |
|------|---------|
| `--entry FILE` | Entry-point file relative to the build dir (default `main.py`) |
| `--python PATH` | Python interpreter (default `python3`) |
| `--debugger MODULE` | Module to launch under `python -m` (default `pdb`; e.g. `pudb`, `ipdb`, `debugpy`) |
| `--break TY:LINE` | Set a breakpoint at `<ty-file>:<line>` (Windows-style `C:\foo.ty:10` and POSIX paths both work — the line number is parsed from the segment after the last `:`). Repeatable. Lines that don't appear in the `.py.map` table surface a warning and are skipped. |
| `--no-build` | Skip rebuilding; assume `build/` is current |

## `tyc run`

Executes a Typhon program. Two execution modes:

- **VM (default).** Runs the source in the in-process tree-walking interpreter from `tyc-vm`. No `.py` is written, no CPython is spawned. See [docs/vm.md](vm.md) for the supported feature surface.
- **`--compile`** (alias `--no-vm`). Falls back to the legacy "build then exec CPython" path — required when your program imports CPython libraries the VM doesn't speak natively (`numpy`, `requests`, `pandas`, …).

Both modes type-check before executing. VM mode runs `tyc check` directly; `--compile` mode runs the full `tyc build` pipeline (which includes the check). The VM used to skip the static pass and crash with a Python-style `NameError` on programs that should have surfaced `tyc::unknown_name`; v0.3.1 gates VM execution behind the check pipeline, so unresolved names / type errors / arity mismatches fail the same way they would under `tyc check` or `tyc build`. The `TYC_SKIP_CHECK=1` env var disables the pre-VM check for the rare case where you want the legacy run-only-the-VM behaviour (mostly: probing the VM against deliberately-broken inputs in stress harnesses). `--compile` has no equivalent bypass — the build pipeline always type-checks.

**Multi-file projects (v0.9.0).** The VM loads sibling `.ty` modules from the project source root on demand, honours relative imports (`from .repo import x`, `from ..pkg.users import load`), and caches each module's bindings as a `Value::Module`. `tyc run --compile` now spawns `python -m <pkg>.main` instead of `python build/main.py` so relative imports in the entry point resolve correctly under the compiled path too. From v0.8.0 onwards `--compile` rejects single-file inputs up-front with an actionable error — compile mode requires the `typhon.toml`-driven layout because the emitter writes `build/main.py` + `build/typhon_runtime/...` alongside.

**VM coverage parity with the compiled path (v0.9.0).** The VM now handles `Result` combinators (`.map` / `.map_err` / `.and_then` / `.or_else` on `Ok` / `Err`), write/append/binary `open()` modes (plus `__enter__` / `__exit__`), class patterns on built-in types in `match` (`case str() as s:`, `case int() as n:`), `frozenset` as a dict key, deep `freeze let`, comptime substitution, `lazy import np = numpy`, `class!` exception fields surviving `except X as e:`, `dataclasses.field(default_factory=...)` per-instance factories, and native shims for `collections.deque` / `heapq` / `contextlib.contextmanager` / `pydantic.BaseModel`. See [docs/vm.md](vm.md) for the full surface.

**VM completeness (v0.10.0).** The VM now dispatches dunders and rich comparisons on user instances (`__add__` + reflected forms, `__eq__` / `__lt__` / …, `__str__` / `__repr__` / `__len__` / `__getitem__` / `__contains__`), runs finite generators (`yield` / `yield from`, eagerly materialised, capped at 1M items), models `type(x)` as a real type object (`type(x).__name__`, `type(x) == int`), invokes `@property` getters on attribute read and binds `cls` for `@classmethod` (both inherited through bases). The long tail of builtins lands — `divmod`, `pow` (2- and 3-arg), `format`, `ascii`, `int(str, base)` (incl. `base=0`), full set algebra, the missing string methods, `json.dumps(indent=…)`, `time.perf_counter` / `process_time`, `math.gcd` / `lcm` / `factorial` / `isqrt` / `comb` / `perm` — and `max` / `min` / `list.sort` accept `key=` / `reverse=` / `default=` kwargs. Pydantic `model_validate` / `model_dump` / `model_dump_json` make flat `model` classes usable under `tyc run`. Lazy / unbounded generators and `@contextmanager` generators inside `with` blocks still need `--compile`. See [docs/vm.md](vm.md) for the full surface.

**VM parity sweep + `enum` (v0.11.0).** Closes 22 findings from a fresh v0.10.0 stress round. The new `enum Name:` keyword sugars over `enum.Enum` with `enum.auto()` for bare members. Two new VM value kinds land: `Value::Complex` (native complex arithmetic with promotion across int / float, hashable for dict / set keys) and a dict-view kind backing `dict.keys()` / `.values()` / `.items()` (repr / iterate / `in` / `len` match CPython). Bare `super()` is rewritten by `tyc-desugar` to the explicit two-arg `super(EnclosingClass, self)` form so `@dataclass(slots=True)` no longer crashes; `__call__` dispatches on callable instances; `__post_init__` fires after auto-generated construction; multi-level inheritance accumulates fields across the full MRO. New / expanded stdlib shims: native `enum` / `datetime` (naïve / UTC) / `pathlib` (`/` join, `.suffixes`, `.parts`) / `collections.defaultdict` (factory invoked via subscript `__missing__`). Real `re.Match.group` / `.groups` / `.groupdict` capture groups; banker's `round`; `bytes` methods (`decode` / `hex` / `fromhex` / …); `itertools.groupby(key=)`; `str.split(maxsplit=)` as a pure-keyword arg; f-string `{x=}` debug conversion; `str %` / f-string `%` runtime formatting. **VM value semantics align with CPython** — dataclass instance eq / repr / hash is value-based (keyed on class identity, not name), set / frozenset equality is order-independent, float repr matches CPython's shortest round-tripping form. The type checker tightens: `None` flows into `object`, `str %` is type-checked, and `(5).items()` / `5["a"]` / `for x in 5:` fire at check time. `tyc init` seeds `allow-secret-comptime = false` in the generated `typhon.toml`. See [docs/vm.md](vm.md) and [CHANGELOG.md](../CHANGELOG.md#0110--2026-06-04) for the full surface.

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
