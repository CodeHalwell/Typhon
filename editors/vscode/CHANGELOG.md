# Typhon VS Code extension — changelog

## 0.2.4

- Security: a `typhon.server.path` that contains a path separator must now be
  absolute. A relative value (`./bin/tyc`, `tools\\tyc.exe`) used to be
  resolved against the extension host's working directory; it is now rejected
  with a warning that asks for an absolute path. Bare command names (`tyc`)
  are still looked up on PATH.
- Security: the extension declares
  `capabilities.untrustedWorkspaces.supported: false`, so it stays disabled in
  Restricted Mode — the language server it launches executes project tooling.
- Dependencies: `brace-expansion` updated past the advisory range (transitive
  of `vscode-languageclient`).

## 0.2.3

- Fixed `SCREAMING_CASE` constants (e.g. `AZURE_OPENAI_ENDPOINT`) being
  highlighted as types — they now use the constant scope
  (`variable.other.constant`) instead of the class/type colour.
- Fixed keyword/named arguments whose name collides with a Typhon soft
  keyword (`model=`, `enum=`, `with=`, `type=`, `match=`) being coloured as
  keywords; an `ident=` argument now scopes as `variable.parameter`, matching
  its sibling kwargs.

## 0.2.2

- Syntax highlighting for the `rescue` exception-boundary keyword (postfix and
  block forms) and the `as!` checked boundary cast.
- README keyword list updated to mention `rescue` and `as!`.

## 0.2.1

- Syntax highlighting for `.ty` / `.dty` (bindings, modifiers, constructs,
  visibility, sugar, Result types), LSP integration via `tyc lsp`
  (diagnostics, hover, go-to-definition, completion), and editor configuration.
