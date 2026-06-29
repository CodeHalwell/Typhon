# Typhon VS Code extension — changelog

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
