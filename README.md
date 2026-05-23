<div align="center">
  <img src="docs-site/src/assets/typhon-logo.png" alt="Typhon — type-safe, optimised, compiles to CPython" width="480" />

  <h1>Typhon</h1>

  <p><strong>A statically-typed, stricter superset of Python that compiles to clean, readable CPython 3.13+ — with no runtime dependency on the toolchain.</strong></p>

  <p>
    <a href="https://github.com/codehalwell/Typhon/actions/workflows/ci.yml"><img src="https://github.com/codehalwell/Typhon/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="https://github.com/codehalwell/Typhon/actions/workflows/deploy-docs.yml"><img src="https://github.com/codehalwell/Typhon/actions/workflows/deploy-docs.yml/badge.svg" alt="Docs"></a>
    <a href="https://github.com/codehalwell/Typhon/actions/workflows/vscode-extension.yml"><img src="https://github.com/codehalwell/Typhon/actions/workflows/vscode-extension.yml/badge.svg" alt="VS Code Extension"></a>
    <a href="https://codehalwell.github.io/Typhon/"><img src="https://img.shields.io/badge/docs-codehalwell.github.io%2FTyphon-2563eb" alt="Documentation"></a>
    <img src="https://img.shields.io/badge/license-MIT-yellow.svg" alt="License: MIT">
    <img src="https://img.shields.io/badge/rust-1.94-orange.svg" alt="Rust 1.94">
    <img src="https://img.shields.io/badge/python-3.13%2B-blue.svg" alt="Python 3.13+">
  </p>

  <p>
    <a href="https://codehalwell.github.io/Typhon/"><strong>Documentation</strong></a> ·
    <a href="docs/language.md">Language Reference</a> ·
    <a href="docs/cli.md">CLI Guide</a> ·
    <a href="docs/roadmap.md">Roadmap</a>
  </p>
</div>

---

> Every `.ty` file emits valid, idiomatic `.py`. Not all `.py` is valid Typhon.

The compiler and language server live in a single Rust binary called `tyc`.

## Install

Pre-built binaries ship on every release for **macOS (Apple Silicon + Intel)**, **Linux (x86_64 + aarch64, glibc)**, and **Windows (x86_64)**.

**macOS / Linux:**

```bash
curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh
```

**Windows (PowerShell):**

```powershell
iwr -useb https://raw.githubusercontent.com/codehalwell/typhon/main/install.ps1 | iex
```

The installers detect your platform, resolve the latest release from the GitHub API, verify the SHA-256 checksum, and drop `tyc` into a per-user directory (`$HOME/.local/bin` on macOS/Linux; `%LOCALAPPDATA%\Programs\Typhon` on Windows). On macOS the Gatekeeper quarantine attribute is cleared automatically; on Windows the install directory is added to the user-level `PATH`.

If you download an archive manually on macOS, clear the Gatekeeper quarantine attribute once before the first run:

```bash
xattr -d com.apple.quarantine ~/.local/bin/tyc
```

`tyc` itself runs anywhere a modern Rust toolchain produces a binary; the emitted Python **requires CPython 3.13 or newer** at runtime. Older targets are rejected at `typhon.toml` load time so projects can't accidentally build code their interpreter won't run.

For a longer-form guide (custom install dir, pinned version, uninstall, manual download, troubleshooting), see [docs/install.md](docs/install.md).

## Why

- **Static safety** — non-nullable by default, no implicit `Any`, explicit error handling via `Result[T, E]`.
- **Modern ergonomics** — `let`/`mut`, interfaces, sealed unions with exhaustive matching, guards, pipes, comptime, lazy loading.
- **Clean output** — emits idiomatic Python; deploy like any other Python project, no Typhon runtime to install.
- **First-class tooling** — one binary builds, checks, formats, and runs as an LSP with sub-100 ms incremental feedback.

## Documentation

📖 **The full documentation site lives at [codehalwell.github.io/Typhon](https://codehalwell.github.io/Typhon/)** — browsable guides, references, and tutorials generated from [`docs-site/`](docs-site/).

The canonical design doc is **[the long-term plan](docs/long-term-plan.md)** — goals, architecture, language design, roadmap, and risks in one place.

Focused references:

| | |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Compiler pipeline, crate layout, toolchain choices |
| [docs/language.md](docs/language.md) | Type system, error handling, async, `let`/`mut`, comptime |
| [docs/cheatsheet.md](docs/cheatsheet.md) | 30-second syntax refresher (also `tyc cheatsheet`) |
| [docs/vm.md](docs/vm.md) | The in-process tree-walking VM (default execution mode for `tyc run`) |
| [docs/cli.md](docs/cli.md) | The `tyc` binary and its subcommands |
| [docs/configuration.md](docs/configuration.md) | `typhon.toml` reference |
| [docs/diagnostics/](docs/diagnostics/) | One page per `tyc::` code — also surfaced via `tyc explain <code>` |
| [docs/roadmap.md](docs/roadmap.md) | Phased delivery plan |
| [docs/risks.md](docs/risks.md) | Risks and mitigations |
| [docs/prior-art.md](docs/prior-art.md) | TypeScript, rust-analyzer, ty, Pyrefly, oxc, Ruff |
| [editors/vscode/README.md](editors/vscode/README.md) | VS Code extension — syntax highlighting and LSP client |

## Quick start

```bash
# Build the compiler
cd tyc
cargo build --release

# Scaffold a new project
./target/release/tyc init myapp

# Check and format Typhon source
./target/release/tyc fmt src/
./target/release/tyc check src/
```

## The `tyc` binary

| Command | Purpose |
|---------|---------|
| `tyc build` | Full pipeline: parse, check, analyse, desugar, emit, format. `--check` dry-runs the pipeline and lists every would-be-written file without touching disk |
| `tyc check` | Parse and type-check only — no emission. For CI use |
| `tyc fmt` | Format `.ty` source files in place. Runs the in-process whitespace normaliser and then pipes through `ruff format` when on `PATH` |
| `tyc lsp` | Run as a Language Server on stdio |
| `tyc init` | Scaffold a new project: `typhon.toml`, `src/`, `tests/`. The generated `src/main.ty` ships a frozen class + `impl` block + `Result`/`?`/`match` example |
| `tyc trace` | Map a Python traceback back to Typhon source via `.py.map` v2 source maps (per-statement `(out_line → ty_line)` table emitted by the printer) |
| `tyc profile` | Build then instrument every top-level function with call-count + wall-clock sampling; writes `typhon-profile.json` on interpreter exit |
| `tyc migrate` | Convert typed Python (`.py`) into Typhon (`.ty`): `Optional[T]`/`T \| None` → `T?`, module-level annotated assigns gain `let`/`mut`, `@dataclass` decorators and `from dataclasses import dataclass` are dropped |
| `tyc ty`      | Build the project and run Astral's `ty` checker against the emitted Python. Requires `ty` to be installed separately (`pip install ty`). |
| `tyc stubtest` | Build the project and run `python -m mypy.stubtest` against every emitted `.pyi`. Complements `tyc check --stubs` (AST diff) by catching dynamically-created members via runtime introspection. Requires `mypy` (`pip install mypy`). |
| `tyc repl`    | Interactive Typhon evaluator. Compiles each block through the full pipeline and executes against a Python interpreter |
| `tyc run`     | Execute a Typhon program. Defaults to the in-process `tyc-vm` tree-walking interpreter ([docs/vm.md](docs/vm.md)) — no `.py` is written, no CPython is spawned. `--compile` (alias `--no-vm`) falls back to the legacy build-then-exec path |
| `tyc debug`   | Build the project and launch the emitted Python under a debugger (default `pdb`). Repeatable `--break <ty-file>:<line>` translates Typhon source locations through `.py.map` and forwards them to the debugger |
| `tyc explain` | Print the diagnostic catalog entry for a `tyc::` code (mirrors `rustc --explain`). Catalog pages live under `docs/diagnostics/` and are linked from the `url(...)` clause on every diagnostic |
| `tyc cheatsheet` | Print the 30-second Typhon cheat sheet ([docs/cheatsheet.md](docs/cheatsheet.md)) to stdout |
| `tyc add` / `tyc remove` / `tyc sync` | Lightweight package-manager surface over `uv`: rewrite `[dependencies]` / `[dev-dependencies]` in `typhon.toml` and run `uv sync` to install |

See [docs/cli.md](docs/cli.md) for the full reference.

## Project status

**Current release: [v0.5.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.5.0)** — the post-v0.4 roadmap-sweep release that closes seven Phase 4+ items and the four follow-up epics flagged during that sweep. Highlights since v0.4.0:

- 🟢 **Salsa cache shared across `check_file_with_imports`.** New `preprocessed_full` tracked query backs the LSP's per-keystroke cross-module check; cached parse + resolve hits on every unchanged sibling. `check_source_file_with_imports` no longer double-resolves to harvest diagnostics — `ArcResolvedModule` now carries both behind `Arc` for pointer-equality `salsa::Update`.
- 🟢 **`tyc debug` reads in full Typhon coordinates.** The pdb subclass overrides `do_list`, `do_where`, `format_stack_entry`, and the `prompt` property so `where`, `list`, the prompt, and frame summaries all show `.ty` paths and source slices instead of the emitted `.py`. `tyc ty`'s diagnostic remapper handles paths with spaces by walking left and taking the longest candidate that resolves to a real `.py.map`.
- 🟢 **Auto-parallel dict-comprehensions.** `{k_expr: f(v) for k, v in items}` rewrites to a dict-literal unpack form alongside the existing list-comp + set-comp coverage. Only fires when exactly one side is a pure-call eligible expression.
- 🟠 **Higher-Kinded Types foundation.** `Type::TypeConstructor { name, arity }` represents type constructors with unbound parameters; `class Functor[F[_]]:` and `def map[F[_], A, B](...)` parse and resolve through the new variant. The full unification surface is staged on this scaffold; see [`TYPE_SYSTEM_FRONTIER.md`](TYPE_SYSTEM_FRONTIER.md) for what remains deferred.
- 🟠 **`comptime` types-as-values.** `comptime let T: type = int` round-trips through the comptime evaluator via the new `ComptimeValue::Type` variant. Bare-name resolution covers the eight primitive heads; `Any` is intentionally rejected unless imported because the emitter cannot synthesise the import.
- 🟢 **`tyc migrate` rewrites three new shapes.** `@dataclass(frozen=True)` → `class X frozen:`, `class X(Protocol):` → `interface X:`, and module-level `NewType("X", T)` → `newtype X = T`. The matching `from typing import …` entries are pruned alongside.
- 🟢 **Corpus coverage: PyPI sweep + drift round 4.** `stress/pypi-sweep/sweep.py` pip-installs typed packages into a tempdir, runs `tyc migrate` + `tyc build`, and semantic-diffs smoke-script output. A fourth `python_semantic_drift` audit round in `stress/round-2026-05-23-drift-round-4/` ships 17 fresh probes (walrus-in-comp, augmented-assign narrowing, `yield from`, match `*`, …) — all green.
- 🟢 **Inter-procedural field-init audit design.** `stress/interprocedural-audit-design.md` records the summary-IR sketch for generalising the trivial-factory audit to multi-step factories that finish initialisation across a call chain.

Full release notes: [CHANGELOG.md](CHANGELOG.md#050). The four follow-up epics that landed in this release are documented in PRs #110 (corpus), #111 (DX / tooling), #112 (incremental compilation), and #113 (HKT + comptime types). Detail on what's still open beyond v0.5.0 lives in [`TYPE_SYSTEM_FRONTIER.md`](TYPE_SYSTEM_FRONTIER.md) and [`docs/roadmap.md`](docs/roadmap.md).

---

**Phase 0 — Foundation** substantially complete:

- ✅ Cargo workspace skeleton with `crates/` directories
- ✅ `let`/`mut` keyword tokens (immutable and mutable bindings)
- ✅ `tyc fmt` — parses and validates `.ty` source, normalises whitespace
- ✅ `tyc check` — validates syntax and emits miette diagnostics
- ✅ `tyc init` — scaffolds new projects with `typhon.toml`
- ✅ Ruff parser fork vendored under `tyc/vendor/` with first-class
  `let`/`mut` soft-keyword support and a `Mutability` field on assignment
  AST nodes. All consumer crates now use `ruff_python_ast` via
  `tyc_syntax::parse_module`; the migration off `rustpython-parser` is
  complete — see [`tyc/vendor/README.md`](tyc/vendor/README.md) for details.
- ✅ `tyc ty` — optional integration that builds the project and runs
  Astral's [`ty`](https://github.com/astral-sh/ty) type checker against
  the emitted Python for a second opinion.

**Phase 1 — Core types** complete:

- ✅ Salsa db with `preprocessed_text` and `module_decl_names` queries
- ✅ Name resolution and scope construction; `let`/`mut` enforcement
- ✅ Nominal types: function signatures, assignment compatibility, primitives, classes
- ✅ Non-nullable by default with flow narrowing on `is None` / `is not None` / `isinstance`
- ✅ `T?` syntax sugar for `T | None` in annotations
- ✅ `tyc check` emits "unknown name", "type mismatch", and "nullable use" diagnostics via miette

**Phase 2 — Class and value features** complete:

- ✅ Class emission as `@dataclass(slots=True)`; `model` keyword emits Pydantic with `extra='forbid'` by default
- ✅ Sealed unions and exhaustive `match`
- ✅ `Result[T, E]`, `Ok`/`Err` constructors, generated `typhon_runtime.py` helper
- ✅ `?` operator for `Result` error propagation (context-checked against the enclosing function's return type)
- ✅ `with`-chain Result sequencing with optional `else err:` block
- ✅ `comptime let/mut` with `env()` lookup; required env vars declared in `[env]`
- ✅ `impl` blocks merged into class definitions at desugar
- ✅ `tower-lsp-server` LSP backend: `tyc lsp` publishes diagnostics on `did_open` / `did_change`, serves position-based hover (binding kind + mutability), and answers go-to-definition by jumping to the resolver's recorded declaration site.

**Phase 3 — Structural typing and advanced features** complete:

- ✅ Generics syntax locked to PEP 695 (`def f[T](x: T)`, `type Vec[T] = list[T]`). Type params resolve in scope and are preserved as `Type::TypeVar(name)` through signatures. Call-site bidirectional inference binds typevars from actual arguments (recursively, e.g. `list[T]` against `list[int]` infers `T=int`; conflicting bindings widen to a union) and substitutes them in the return type. Multi-arg constraint solving and bounded type-var checking are wired up; full variance and higher-kinded forms remain partial.
- ✅ `interface Name:` lowers to `class Name(Protocol):` with a structural conformance check on assignment that walks the candidate's MRO and matches field types. `isinstance(x, Interface)` is rejected by default.
- ✅ `unsafe:` lexical region: lowers to `if True:` for scope preservation, and the type checker tracks `unsafe_depth` to suppress diagnostics inside the block so users can interface with untyped Python without fighting the checker. Boundary checks at assignment sites outside the block apply normally.
- ✅ `@pure`/`@memo`/`@pure(memo=True)` decorators trigger the six-condition purity check; memoised functions get `@functools.cache` injected at desugar time. Project-wide opt-in via `[strictness] auto-memoise`.
- ✅ `gather:` lowers to `asyncio.TaskGroup` by default; `gather(strategy="best-effort"):` to `asyncio.gather(..., return_exceptions=True)`.
- ✅ `go f(x)` lowers through `typhon_runtime.tasks.spawn` with a strong-ref task registry.
- ✅ `lazy import np = numpy` lowers to a thread-safe inline proxy class; `lazy from … import …` is rejected. Module-level `lazy let NAME: T = expr` lowers to a sentinel-cached `lazy_let(lambda: expr)`; class-body `lazy let` lowers to `@cached_property`.
- ✅ Pipe operator `a |> f |> g(arg)` desugars to `g(f(a), arg)`.
- ✅ `extend ClassName:` (alias for `impl` on user-defined classes); `extend BUILTIN:` for `str`/`list`/`dict`/… extracts each method to a module-level free function and rewrites call sites whose receiver carries a matching static annotation. No monkey-patching of built-ins.
- ✅ `.dty` stub files compile to PEP 561 `.pyi`. `tyc check --stubs` parses every `.dty` and diffs its surface API (functions, classes, methods, annotated fields, parameter shapes) against the sibling `.ty`/`.py` implementation, emitting `tyc::stub_mismatch` diagnostics for missing-in-impl / missing-in-stub / signature-mismatch findings. A runtime introspection probe (mypy's `stubtest` proper) is still a follow-up.

**Phase 6 — Python-annoyances surface** complete ([v0.3.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.3.0), correctness follow-ups in [v0.3.1](https://github.com/CodeHalwell/Typhon/releases/tag/v0.3.1), [v0.4.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.4.0), and [v0.5.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.5.0)):

- ✅ **`newtype Name = Base`** — TypeScript-style nominal aliases over primitives. `UserId` flows freely into an `int` slot, but a bare `int` requires explicit `UserId(x)` construction to satisfy a `UserId`-typed target. Compiles to a zero-cost `typing.NewType` call. New `tyc::newtype_violation` diagnostic.
- ✅ **`freeze let X = expr`** — deep-immutable bindings. Wraps the RHS in `typhon_runtime.freeze.deep_freeze`, recursively replacing `list → tuple`, `dict → MappingProxyType`, `set → frozenset` so the value (not just the name) is locked.
- ✅ **`pub` modifier** — module-level visibility marker. When any name is `pub`, desugar synthesises a top-of-file `__all__ = [...]` list so `from foo import *`, Sphinx autoapi, and IDE re-export filters all see the public surface.
- ✅ **`tyc::blocking_in_async`** — flags direct calls to known-blocking stdlib functions (`time.sleep`, `requests.get`, `subprocess.run`, `input`, …) inside `async def` bodies.
- ✅ **`tyc::resource_not_managed`** — flags bare assignments of `open` / `socket.socket` / `sqlite3.connect` / `tempfile.*` calls that aren't wrapped in a `with` statement.
- ✅ **`tyc::div_by_zero_literal`** — catches `x / 0`, `x // 0`, `x % 0` at compile time when the divisor is a literal.
- ✅ **Cross-platform install** — pre-built `tyc` binaries for Linux (x86_64 + aarch64) and Windows (x86_64) join the macOS matrix. New `install.ps1` PowerShell installer for Windows; the existing `install.sh` now detects Linux and chooses the matching tarball.
- ✅ **Findings sweep** — every open finding from the May 2026 stress campaigns (O2–O29) closed or verified-fixed. `tyc fmt` gains five PEP 8 rules, TypedDict-style dict literals type-check against class shapes, inline `?` works (`Ok(add(parse(s)?, parse(t)?))`), `Sized`-style Protocols match built-in containers, `tyc migrate` rewrites `Union[T, None]` → `T?`, VM Result repr matches CPython, and more — see [CHANGELOG.md](CHANGELOG.md) for the full list.

**Phase 5.5 — Constructor / method arity safety** complete ([v0.2.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.2.0)):

- ✅ `tyc::arg_count` now fires on **class constructors**: a `class ApiClient: api_key: str` instantiated as `ApiClient(base_url="…")` is rejected at `tyc check` / `tyc build` time instead of crashing at runtime with `TypeError: missing 1 required positional argument`. Required fields, `T?` without `= None`, and `model` (Pydantic) classes are all checked.
- ✅ `tyc::arg_count` now fires on **`impl` methods**: `u.greet()` is flagged when `greet` declares a required `prefix: str` parameter. Previously method calls fell into the permissive arity shape.
- ✅ **Cross-module arity propagation:** `from foo import ApiClient` and `import foo as f; f.ApiClient(…)` both flow through the new arity checks. `.ty` source and `.dty` stubs participate on equal footing (stubs win on collisions). Works in `tyc check`, `tyc build`, and the LSP.
- ✅ **Salsa-cached LSP shape extraction:** a new `tyc_db::module_shapes_query(file)` salsa-tracked query caches per-file shape extraction. A keystroke in one file only re-runs extraction for that file.
- ✅ **`tyc::missing_field_init`** post-construction audit: catches the `X.__new__(X)` / `object.__new__(X)` bypass pattern when the instance escapes the function (return / call argument) without every required field assigned. Dropped conservatively on `setattr`, on method calls, and inside `unsafe:` regions.

**Phase 5 — Interop and developer experience** complete ([v0.1.6](https://github.com/CodeHalwell/Typhon/releases/tag/v0.1.6)):

- ✅ `plain class X:` keyword for "regular Python class, no `@dataclass`, no synthesised `__init__`" — the symmetric escape hatch alongside `frozen class` and `class!`.
- ✅ Auto-skip `@dataclass` injection for `Enum` / `IntEnum` / `StrEnum` / `Flag` / `IntFlag` / `ABC` / `ABCMeta` subclasses; project-specific bases via `[emit] skip-decoration-bases`.
- ✅ `[emit] class-default` rejects unknown values at config load (`tyc::invalid_config_value`) instead of silently falling back to `"dataclass"`.
- ✅ Python-semantic alignment: `or`/`and` typed as `Union[truthy(lhs), rhs]` (and the falsy dual), generator functions structurally assignable to `Iterable[T]` / `Iterator[T]` / `AsyncIterable[T]` / `AsyncIterator[T]`.
- ✅ `tyc explain <code>` prints diagnostic catalog entries (mirrors `rustc --explain`); `tyc cheatsheet` prints the 30-second syntax refresher.
- ✅ `tyc init` scaffold ships a frozen-dataclass + `impl` block + `Result`/`?`/`match` example, plus a fully-commented `typhon.toml`.
- ✅ `.py` files in `src/` are copied verbatim into the build output; a relative `.py` import that resolves outside `src/` fires `tyc::orphan_py_import`.
- ✅ Diagnostic deep-links: every `tyc::` diagnostic carries a miette `url(https://typhon.dev/lang/diagnostics/<code>)` clause and the 50+ catalog pages under [docs/diagnostics/](docs/diagnostics/) are embedded into the binary for `tyc explain`.
- ✅ `tyc build --check` dry-run mode lists every file that would be written without touching disk; `tyc::contains_secret_literal` flags inlined env values whose binding name matches a credential suffix.
- ✅ `tyc fmt` wraps `ruff format` after the in-process whitespace pass (when `ruff` is on `PATH` and the buffer contains no Typhon-only tokens).
- ✅ `tyc debug --break <ty-file>:<line>` translates Typhon source locations through `.py.map` and injects `-c "break …"` into the chosen debugger session.

**Phase 4+ — Beyond v1** also landed:

- ✅ Automatic `asyncio.gather` inference (opt-in via `[strictness] auto-gather`): straight-line runs of two-or-more independent `await` calls inside an `async def` are folded into a `TaskGroup`.
- ✅ Loop parallelisation for pure list comprehensions on free-threaded Python (opt-in via `[strictness] auto-parallel`, threshold `parallel-min-size`).
- ✅ PGO via `tyc profile` (opt-in via `[strictness] pgo-memoise`): `tyc build` reads `typhon-profile.json` and promotes pure functions whose call counts meet `pgo-min-calls` to `@functools.cache`.
- ✅ LSP completions (visible bindings + Typhon keywords + common builtins + venv-driven member-access introspection + from-import members from sibling files) and a "Remove unused import" code-action quick-fix.
- ✅ Cross-file go-to-definition across `.ty`/`.py` boundaries via the resolver's Salsa-tracked `resolved_module` query.
- ✅ Attribute resolution against class definitions (`obj.method`, `Class.field`) and re-export-aware import resolution.
- ✅ `tyc migrate` (typed Python → Typhon), `tyc repl` (interactive evaluator), `tyc debug` (pdb launcher with `--break`), and `tyc add`/`remove`/`sync` (uv-backed package manager).
- ✅ `tyc-vm`: in-process tree-walking interpreter that runs `.ty` source directly. `tyc run` uses it by default — no `build/`, no CPython process spawn. See [docs/vm.md](docs/vm.md) for the supported feature surface; programs that import CPython-only libraries fall back via `tyc run --compile`.
- ✅ Source-map line accuracy: `.py.map` records a per-statement `(out_line → ty_line)` table consumed by `tyc trace` and `tyc debug --break`.

See [docs/roadmap.md](docs/roadmap.md) for the phased plan, [CHANGELOG.md](CHANGELOG.md) for release-by-release notes, and [docs/findings.md](docs/findings.md) for the consolidated stress-test findings and open follow-ups.

## Configuration

`typhon.toml` at the project root drives every subcommand:

```toml
[project]
name = "myapp"
version = "0.1.0"
src = "src"
out = "build"

[python]
target = "3.13"
free-threaded = false

[emit]
class-default = "dataclass"  # or "pydantic"
format = true

[strictness]
no-implicit-any = true
unused-import = "error"
exhaustive-match = "error"
```

Full reference: [docs/configuration.md](docs/configuration.md).

## Workspace layout

```
tyc/
├── Cargo.toml                  (workspace root)
├── crates/
│   ├── tyc-syntax/             Typhon lexer/parser
│   ├── tyc-db/                 Salsa incremental database
│   ├── tyc-resolve/            Name resolution and scope construction
│   ├── tyc-types/              Nominal type checker with non-null narrowing
│   ├── tyc-analyse/            Purity, async, comptime (Phase 2+)
│   ├── tyc-desugar/            Typhon AST → Python AST (Phase 2+)
│   ├── tyc-emit/               Python codegen
│   ├── tyc-format/             Source formatter (in-process pass + `ruff format` wrap)
│   ├── tyc-diagnostics/        miette-based diagnostics
│   ├── tyc-lsp/                LSP backend
│   ├── tyc-vm/                 In-process tree-walking interpreter (default for `tyc run`)
│   └── tyc/                    CLI binary
└── vendor/                     Vendored crates — Typhon's fork of Ruff
    ├── ruff_text_size/         TextSize/TextRange newtypes
    ├── ruff_source_file/       Line-index over a source string
    ├── ruff_python_trivia/     Whitespace + comment helpers
    ├── ruff_python_ast/        Python AST + Typhon's Mutability extension
    └── ruff_python_parser/     Lexer + parser with let/mut soft keywords
```

See [docs/architecture.md](docs/architecture.md) for the pipeline and crate-by-crate breakdown.
