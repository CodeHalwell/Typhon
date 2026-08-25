# Contributing to Typhon

Thanks for your interest in Typhon! It's an early-stage project (currently
`v1.0.0-alpha`), so contributions, bug reports, and language feedback are all
welcome.

## Ground rules

- **The docs are the source of truth for the language**, and the compiler is
  the source of truth for behaviour. When they disagree, fix the drift.
- **Language changes must be additive on correct programs.** A new diagnostic
  may only fire on code that already failed at runtime — never narrow a program
  that previously type-checked and ran correctly. See `CLAUDE.md`.
- **Every user-facing change lands with a `CHANGELOG.md` entry**, and every new
  `tyc::` diagnostic needs a `docs/diagnostics/<code>.md` page.

## Working on the Rust compiler

All commands run from the `tyc/` workspace root (toolchain pinned in
`tyc/rust-toolchain.toml`):

```bash
cd tyc
cargo build --release            # → tyc/target/release/tyc
cargo test --workspace           # full test suite
cargo fmt -- --check             # formatting gate (CI enforces this)
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

CI treats **all warnings as errors** — keep the tree warning-clean. It runs
five jobs: `test` (fmt → clippy → test), `security` (`cargo-deny`), a
perf gate (`scripts/perf-gate.sh`), the VM↔CPython `differential` gate
(`scripts/vm-differential.sh`), and the opt-in-knob `knob-matrix`
(`scripts/knob-matrix.sh`).

## Working in the Typhon language

```bash
alias tyc="$PWD/tyc/target/release/tyc"
tyc init demo && cd demo
tyc check src/     # parse + resolve + type-check
tyc run            # execute via the in-process VM
tyc build          # emit build/main.py
```

The `examples/` and `stress/` corpora are the regression net — new type-checker
work is expected to keep them green and false-positive-free.

## Pull requests

- Branch from `main` (or a `dev/**` / `claude/**` branch); CI runs on those.
- Keep PRs focused; describe the behaviour change and link any relevant issue.
- Run the four commands above before pushing.

## Reporting bugs and security issues

- Functional bugs: open a GitHub issue with a minimal `.ty` reproduction and the
  `tyc` output (`tyc --version`, the command you ran, the diagnostic or crash).
- Security vulnerabilities: **do not** file a public issue — see
  [SECURITY.md](SECURITY.md).
