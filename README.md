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
    <a href="docs/zero-to-hero/README.md">Zero to Hero</a> ·
    <a href="docs/language.md">Language Reference</a> ·
    <a href="docs/cli.md">CLI Guide</a> ·
    <a href="CHANGELOG.md">Changelog</a>
  </p>
</div>

---

> **Every `.ty` file emits valid, idiomatic `.py`. Not all `.py` is valid Typhon.**

Typhon is to Python what TypeScript is to JavaScript: you write a stricter, safer,
more ergonomic dialect, and the compiler hands you clean CPython that runs anywhere
Python runs — no Typhon runtime, no PyPI dependency, no special interpreter. The
compiler, language server, formatter, REPL, debugger wrapper, and an in-process
interpreter are all one Rust binary, **`tyc`**.

## A taste of Typhon

```python
# A sealed union of error variants — a closed set the checker knows exhaustively.
type LoadError = NotFound | Timeout

class NotFound:  path: str
class Timeout:   after_ms: int

def load(path: str) -> Result[str, LoadError]:      # errors are values, in the signature
    try:
        with open(path) as f:
            return Ok(f.read())
    except FileNotFoundError:
        return Err(NotFound(path=path))

def main() -> None:
    match load("config.toml"):
        case Ok(body):
            print(body)
        case Err(NotFound(path)):
            print(f"missing: {path}")
        case Err(Timeout(after_ms)):
            print(f"timed out after {after_ms}ms")
```

Add a third variant to `LoadError` and **every `match` site turns red** until you
handle it. That is the kind of safety Typhon brings to Python — and it compiles to
idiomatic dataclasses + a small generated `Ok`/`Err` helper you can read and debug.

## Why Typhon

- **Static safety** — non-nullable by default (`T` can't hold `None`; use `T?`), no implicit `Any`, explicit error handling via `Result[T, E]`.
- **Modern ergonomics** — `let`/`mut` bindings, structural interfaces, sealed unions with exhaustive `match`, guards, pipes, `comptime` constants, lazy imports.
- **Clean output** — emits Python you'd be happy to ship; deploy like any other Python project, with no Typhon runtime to install.
- **First-class tooling** — one binary builds, checks, formats, runs, and serves an LSP with sub-100 ms incremental feedback.

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

The installers detect your platform, resolve the latest release, verify the SHA-256 checksum, and drop `tyc` into a per-user directory (`$HOME/.local/bin` on macOS/Linux; `%LOCALAPPDATA%\Programs\Typhon` on Windows). On macOS the Gatekeeper quarantine attribute is cleared automatically; on Windows the install directory is added to your `PATH`.

`tyc` runs anywhere a modern Rust toolchain produces a binary; the emitted Python **requires CPython 3.13 or newer** at runtime. Prefer to build from source? See [Get started](#get-started) below. Longer-form install guide (custom dir, pinned version, uninstall, troubleshooting): [docs/install.md](docs/install.md).

## Get started

```bash
tyc init myapp        # scaffold typhon.toml + src/main.ty + tests/
cd myapp

tyc fmt src/          # normalise whitespace, then wrap `ruff format` when on PATH
tyc check src/        # parse + resolve + type-check — no artifacts (use this in CI)
tyc run               # execute via the in-process VM: no .py written, no CPython spawned
tyc build             # full pipeline → build/main.py (+ source maps)
python build/main.py
```

<details>
<summary>Building <code>tyc</code> from source instead</summary>

```bash
cd tyc
cargo build --release          # → tyc/target/release/tyc
alias tyc="$PWD/target/release/tyc"
```

The toolchain is pinned to Rust **1.94** via `tyc/rust-toolchain.toml`. See [CONTRIBUTING.md](CONTRIBUTING.md) for the full developer workflow.
</details>

## Learn Typhon

LLMs and humans alike have no prior knowledge of Typhon — it looks like Python, but a
handful of rules diverge on purpose. Pick the path that fits how you like to learn:

| Path | Best for | Start |
|---|---|---|
| 🚀 **[Zero to Hero](docs/zero-to-hero/README.md)** | A fast, end-to-end tour — 10 short lessons from install to a capstone program | [Lesson 1 →](docs/zero-to-hero/lesson-01-getting-started.md) |
| 📚 **[Programming guides](docs/guides/README.md)** | Going deep, one feature at a time, with the emitted Python shown side-by-side | [Guide 1 →](docs/guides/01-hello-world.md) |
| 🧪 **[Examples](examples/README.md)** | Learning by reading real code — 32 focused exercises + 15 production-shaped apps | [Browse →](examples/README.md) |
| ⚡ **[Cheat sheet](docs/cheatsheet.md)** | A 30-second syntax refresher (also `tyc cheatsheet`) | [Open →](docs/cheatsheet.md) |

**New here? Read [The eight rules every Typhon program follows](docs/language.md#the-eight-rules-every-typhon-program-follows).**
If you remember nothing else, remember these — everything else is detail. In short:
every parameter and return type is annotated; locals declare `let` or `mut`; `T` can't
be `None`; methods live in `impl` blocks; `Any` only enters through `unsafe:` or `.dty`
stubs; `match` on a sealed union must be exhaustive; errors flow as `Result`, not
exceptions.

> Coming from Python? `tyc migrate src/app.py` rewrites typed Python into Typhon
> (`Optional[T]` → `T?`, `Generic[T]` → PEP 695, drops `@dataclass`, and more) to give
> you a running start.

## The `tyc` toolchain

One binary is the whole workflow. The commands you'll reach for daily:

| Command | Purpose |
|---------|---------|
| `tyc check` | Parse + type-check only, no emission — the recommended CI gate |
| `tyc build` | Full pipeline → `build/*.py` + source maps. `--check` dry-runs it; `-O` turns on the optimisation profile |
| `tyc run` | Execute in the in-process tree-walking VM (no `.py`, no CPython spawn). `--compile` falls back to build-then-exec |
| `tyc fmt` | Normalise `.ty` whitespace, then wrap `ruff format` when on `PATH` |
| `tyc init` | Scaffold a new project (`typhon.toml`, `src/`, `tests/`) with a worked example |
| `tyc lsp` | Run as a Language Server on stdio (diagnostics, hover, go-to-def, completions) |
| `tyc explain <code>` | Print the catalog entry for a `tyc::` diagnostic (offline, like `rustc --explain`) |
| `tyc migrate` | Convert typed Python (`.py`) into Typhon (`.ty`) |

Plus `tyc repl`, `tyc debug`, `tyc trace`, `tyc profile`, `tyc ty`, `tyc stubtest`, and the
`tyc add`/`remove`/`sync` package-manager surface over `uv`. Full reference: [docs/cli.md](docs/cli.md).

## Documentation

📖 **The full documentation site is at [codehalwell.github.io/Typhon](https://codehalwell.github.io/Typhon/)** — browsable guides, references, and tutorials.

Focused references in this repo:

| | |
|---|---|
| [docs/language.md](docs/language.md) | The type system, error handling, async, `let`/`mut`, comptime — the canonical language reference |
| [docs/cheatsheet.md](docs/cheatsheet.md) | 30-second syntax refresher (also `tyc cheatsheet`) |
| [docs/cli.md](docs/cli.md) | Every `tyc` subcommand and flag |
| [docs/configuration.md](docs/configuration.md) | The `typhon.toml` reference |
| [docs/vm.md](docs/vm.md) | The in-process tree-walking VM (default for `tyc run`) |
| [docs/architecture.md](docs/architecture.md) | Compiler pipeline, crate layout, toolchain choices |
| [docs/diagnostics/](docs/diagnostics/) | One page per `tyc::` code — also surfaced by `tyc explain <code>` |
| [docs/roadmap.md](docs/roadmap.md) · [docs/risks.md](docs/risks.md) · [docs/prior-art.md](docs/prior-art.md) | The phased plan, risks, and influences |
| [editors/vscode/README.md](editors/vscode/README.md) | VS Code extension — syntax highlighting + LSP client |

The single canonical design doc is **[the long-term plan](docs/long-term-plan.md)** — goals, architecture, language design, roadmap, and risks in one place.

## Project status

**Current release: [v1.0.0-alpha.9](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha.9)** (2026-08-21).

Typhon reached its **first feature-complete alpha** in
[v1.0.0-alpha](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha): the
proven production surface *plus* the type-system frontier earlier releases deferred
(higher-kinded type unification, user-generic variance inference, the inter-procedural
field-init audit). Since then, the alpha.2 → alpha.9 point releases have been a
soundness, robustness, performance, release-engineering, and codebase-review
hardening pass.

- ✅ **The production path is stable.** `tyc build` → CPython 3.13+ carries no runtime dependency on the toolchain; the full `examples/` + `examples/apps/` corpus builds to runnable Python and checks clean.
- ✅ **The language is additive on *correct* programs** across the whole v0.3.0 → v1.0.0-alpha line — every program that type-checked *and ran correctly* continues to behave identically. (A few deliberate diagnostics reject only code that already crashed at runtime.)
- ⚠️ **As an alpha, the surface syntax is not yet frozen** — it may change before `1.0.0`, always with a documented migration note.
- ⏳ **Deferred to beta:** embedded in-process `ty` (the subprocess `[checker] external = "ty"` path ships), typeshed-backed checking for pure-extension libraries, and the function-level HKT tail.

**Want the details?** The full release-by-release history lives in
**[CHANGELOG.md](CHANGELOG.md)** (every release back to v0.1.0), and the per-feature
status in **[docs/roadmap.md](docs/roadmap.md)**.

## Configuration

`typhon.toml` at the project root drives every subcommand. A minimal file:

```toml
[project]
name = "myapp"
version = "0.1.0"
src = "src"
out = "build"

[python]
target = "3.13"          # required: 3.13+ only

[strictness]
unused-import = "error"
exhaustive-match = "error"
```

Every key — strictness knobs, the `[optimise]` profile, `[emit]` options, `[checker]`, `[env]`, dependencies — is documented in [docs/configuration.md](docs/configuration.md).

## Workspace layout

The compiler is a Cargo workspace under `tyc/`, one crate per pipeline stage:

```
tyc/
├── Cargo.toml                  (workspace root)
├── crates/
│   ├── tyc-syntax/             Typhon lexer/parser (forked Ruff AST + Typhon nodes)
│   ├── tyc-db/                 Salsa incremental database
│   ├── tyc-resolve/            Name resolution and scope construction
│   ├── tyc-types/              Nominal + structural type checker, non-null narrowing
│   ├── tyc-analyse/            Purity, async, comptime, optimisation hints
│   ├── tyc-desugar/            Typhon AST → Python AST
│   ├── tyc-emit/               Python codegen + .py.map source maps
│   ├── tyc-format/             Formatter (in-process pass + `ruff format` wrap)
│   ├── tyc-diagnostics/        miette-based diagnostics
│   ├── tyc-lsp/                LSP backend
│   ├── tyc-vm/                 In-process tree-walking interpreter (default for `tyc run`)
│   ├── tyc-venv/               Venv signature introspection → ModuleShapes
│   └── tyc/                    CLI binary
└── vendor/                     Typhon's fork of Ruff (parser, AST, trivia, source-file)
```

See [docs/architecture.md](docs/architecture.md) for the pipeline and crate-by-crate breakdown, and [CONTRIBUTING.md](CONTRIBUTING.md) to build, test, and hack on it.

## License

Typhon is released under the [MIT License](LICENSE).

It vendors a fork of [Ruff](https://github.com/astral-sh/ruff) under `tyc/vendor/`,
redistributed under Ruff's own MIT license (© 2022 Charlie Marsh) — see
[`tyc/vendor/LICENSE`](tyc/vendor/LICENSE). The release archives ship both notices.

## Security

Typhon is a compiler and language runtime: `tyc build` installs your declared
dependencies (`uv sync`), and `tyc check`/`tyc build`/the LSP introspect and
**import** installed packages to type-check third-party calls — so running them
on an untrusted project executes that project's code, the same trust boundary as
`pip install` or running the code itself. Treat a cloned project as you would any
other untrusted code. To report a vulnerability, see [SECURITY.md](SECURITY.md).
