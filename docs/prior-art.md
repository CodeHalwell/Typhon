# Prior Art

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Typhon stands on a lot of shoulders. These are the languages and tools to study when working on its various stages.

## Languages

- **TypeScript** — closest analogue. Scanner → parser → binder → checker → emitter. The `checker.ts` file is the canonical reference for structural subtyping at scale. The "superset that emits the host language" framing is directly borrowed.
- **Mojo** — cautionary tale. Pitched as a "Python superset," then walked back. **Lesson:** be honest about what subset of Python `.ty` accepts and emit a clean error for the rest.
- **Cython, Coconut, Hy** — older Python supersets. Useful for emission patterns; none built on modern Rust tooling.

## Tools

- **rust-analyzer** — cleanest example of a Salsa-based incremental compiler with an LSP. Crate layering directly transferable.
- **ty and Pyrefly** — Rust-based Python type checkers (Astral and Meta respectively). Both shipped in 2025; both architectural references for Typhon's checker. `ty` may be embedded as a library to handle standard typing-spec checking.
- **oxc** — Rust-based JavaScript toolchain. Workspace layout (`oxc_parser`, `oxc_semantic`, `oxc_linter`, `oxc_formatter`, `oxlint` binary) is the template for `tyc`.
- **Ruff** — Astral's Python linter/formatter. Source of `ruff_python_parser`, `ruff_python_ast`, `ruff_python_codegen`, `ruff_python_formatter`, all vendored.

## Naming

The project ships as **Typhon**. The name keeps phonetic kinship with Python without sounding like a portmanteau, and the mythology lines up (Typhon is the serpent-monster of Hesiod, sometimes treated as the father of Python). The binary is `tyc`, the file extension is `.ty`, the stub extension is `.dty`, the config file is `typhon.toml`.
