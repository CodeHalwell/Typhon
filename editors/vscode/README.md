# Typhon for Visual Studio Code

Language support for [Typhon](https://github.com/codehalwell/typhon) — a statically-typed, stricter superset of Python that compiles to clean CPython 3.13+.

## Features

- **Syntax highlighting** for `.ty` and `.dty` files, including Typhon-specific keywords:
  - Bindings: `let`, `mut`, `val`, `var`
  - Modifiers: `comptime`, `lazy`, `unsafe`, `pure`, `memo`
  - Constructs: `impl`, `interface`, `model`, `extend`, `gather`, `go`, `guard`, `with`-chains
  - Sugar: `T?` nullable, `?` try operator, `|>` pipe
  - Result types: `Result`, `Ok`, `Err`
- **Language server integration** via `tyc lsp`:
  - Diagnostics on save and change
  - Hover (binding kind + mutability)
  - Go-to-definition
  - Completion (Typhon keywords + visible bindings)
- **Editor configuration**: 4-space indent, `#` comments, bracket matching, indentation rules for `def`, `class`, `match`, `impl`, `gather`, etc.

## Requirements

The extension assumes the `tyc` binary is on your `PATH`. To build it:

```bash
cd tyc
cargo build --release
# then add tyc/target/release to PATH, or set `typhon.server.path`
```

If `tyc` is not found the extension falls back to grammar-only highlighting.

## Settings

| Setting | Default | Description |
|---|---|---|
| `typhon.server.path` | `tyc` | Path to the `tyc` binary. |
| `typhon.server.arguments` | `["lsp"]` | Arguments passed to `tyc`. |
| `typhon.server.enable` | `true` | Disable to use grammar-only highlighting. |
| `typhon.trace.server` | `off` | LSP trace level for debugging. |

## Commands

- **Typhon: Restart Language Server** — reload after rebuilding `tyc`.

## Development

```bash
cd editors/vscode
npm install
npm run compile
# Press F5 in VS Code to launch an Extension Development Host
```

To package a `.vsix`:

```bash
npm install -g @vscode/vsce
vsce package
```

## License

MIT — see the [repository root](https://github.com/codehalwell/typhon).
