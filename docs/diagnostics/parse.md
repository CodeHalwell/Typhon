# tyc::parse

Fires when the source file cannot be parsed as a valid Typhon/Python program.
The message describes the specific syntax error and the label points at the
offending token.

## Example

```ty
def main() -> None
    print("missing colon")  # error: parse error — expected `:`
```

## Why

Every later compiler pass assumes a well-formed AST as input. A parse error
is a hard stop because the surrounding token sequence is ambiguous and any
attempt to continue would produce cascading nonsense diagnostics.

## Fix

Read the parse-error message, fix the syntax at the indicated position, and
re-run the build. Most parse errors are missing punctuation (colons,
parentheses, commas), unexpected indentation, or stray keywords.

```ty
def main() -> None:
    print("ok")
```

See https://typhon.dev/lang/diagnostics/parse
