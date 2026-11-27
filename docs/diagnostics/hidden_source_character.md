# tyc::hidden_source_character

A warning for a character in a `.ty` file that makes the source *display*
differently from how it *compiles*:

- a **bidirectional-control character** — the embeddings, overrides and
  isolates U+202A–U+202E and U+2066–U+2069;
- a **carriage return that is not part of a CRLF line ending**.

## Example

A bidirectional override inside a comment or string can make code appear to be
commented out (or the reverse). This is the "Trojan Source" attack,
CVE-2021-42574. In an editor the line below reads as a comparison against
`"user"` followed by a comment; the compiler sees a different string.

```typhon
if access_level != "user<U+202E> <U+2066># Check if admin<U+2069> <U+2066>":
    grant()
```

```text
Warning: tyc::hidden_source_character

  ⚠ bidirectional control character U+202E: the text around it is displayed
  │ in a different order from the one it is compiled in
  help: delete the character; if a string genuinely needs it, write it as an
        escape (`\u202e`, `\r`) so it is visible in the source
```

A lone carriage return works the same way with line breaks. Python (and so
Typhon) ends a line at `\r`; most editors and diff views do not, so

```text
# just a harmless comment<CR>print("hidden")
```

is shown as one comment line and compiled as a comment followed by a call.

## Why a warning

Right-to-left text is legitimate in strings and comments, and the directional
*marks* (U+200E, U+200F, U+061C) that such text normally needs are not
reported. The controls listed above are the ones that can reorder text across
token boundaries; real prose rarely needs them, and when it does an escape
sequence says so explicitly. The check never fails a build — it makes the
character visible to whoever reviews the change.

## Fix

Delete the character. If the string really must contain it, write the escape
(`"\u202e"`, `"\r"`), which compiles to the same value and is visible in
review. CRLF line endings are unaffected.

Added in the 2026-11 security review.
