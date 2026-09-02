#!/usr/bin/env python3
"""Regenerate the VM's embedded Unicode character-property tables.

Emits ``tyc/crates/tyc-vm/src/unicode_data.rs`` from the hosting CPython's own
answers, so the VM's ``str.isdecimal()`` / ``isdigit()`` / ``isnumeric()`` /
``isprintable()``, its ``repr()`` escaping and its ``str.title()`` agree with
the interpreter that runs the compiled program. Rust's std exposes none of
these properties (``char::is_numeric`` is a different, broader question, and
there is no titlecase mapping at all), so the tables are embedded directly.

Run from the repo root::

    python3 scripts/gen-unicode-props.py

Regenerate against the CPython version that hosts the target runtime; the
module header records the Unicode version captured.
"""

from __future__ import annotations

import unicodedata
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "tyc/crates/tyc-vm/src/unicode_data.rs"
MAX = 0x110000


def ranges(pred) -> list[tuple[int, int]]:
    """Inclusive `(lo, hi)` runs of code points satisfying `pred`."""
    out: list[tuple[int, int]] = []
    start: int | None = None
    for cp in range(MAX):
        if pred(cp):
            if start is None:
                start = cp
        elif start is not None:
            out.append((start, cp - 1))
            start = None
    if start is not None:
        out.append((start, MAX - 1))
    return out


def esc(s: str) -> str:
    """A Rust string literal: ASCII printable stays literal, the rest escapes."""
    out = []
    for ch in s:
        c = ord(ch)
        if 0x20 <= c < 0x7F and ch not in ('"', "\\"):
            out.append(ch)
        else:
            out.append("\\u{%X}" % c)
    return '"' + "".join(out) + '"'


def range_table(name: str, doc: str, rows: list[tuple[int, int]]) -> str:
    body = []
    for i in range(0, len(rows), 5):
        chunk = ", ".join(f"(0x{lo:04X}, 0x{hi:04X})" for lo, hi in rows[i : i + 5])
        body.append(f"    {chunk},")
    return (
        f"{doc}\n#[rustfmt::skip]\npub(crate) static {name}: &[(u32, u32)] = &[\n"
        + "\n".join(body)
        + "\n];\n"
    )


def main() -> None:
    decimal = ranges(lambda cp: chr(cp).isdecimal())
    digit = ranges(lambda cp: chr(cp).isdigit())
    numeric = ranges(lambda cp: chr(cp).isnumeric())
    unprintable = ranges(lambda cp: not chr(cp).isprintable())
    space = ranges(lambda cp: chr(cp).isspace())
    title = [
        (cp, chr(cp).title()) for cp in range(MAX) if chr(cp).title() != chr(cp).upper()
    ]

    parts = [
        f'''//! Unicode character properties the VM needs and Rust's std does not have.
//!
//! `str.isdecimal()`, `isdigit()` and `isnumeric()` are three *different*
//! questions in Python (`"²"` is a digit but not a decimal; `"½"` is numeric
//! but neither), `isprintable()` drives both that predicate and `repr()`'s
//! escaping, and there is no titlecase mapping in std at all — the digraphs
//! (`ǆ` → `ǅ`, not `Ǆ`) and `ß` → `Ss` need one.
//!
//! GENERATED FILE — do not edit by hand. Regenerate from the CPython that hosts
//! the target runtime; captured here from Unicode {unicodedata.unidata_version}.
//! See `scripts/gen-unicode-props.py` for the generator.
''',
        range_table(
            "DECIMAL_RANGES",
            "/// Code points for which `str.isdecimal()` is true (general category Nd).",
            decimal,
        ),
        range_table(
            "DIGIT_RANGES",
            "/// Code points for which `str.isdigit()` is true — the decimals plus the\n"
            "/// superscripts and other digit-valued forms that are not positional.",
            digit,
        ),
        range_table(
            "NUMERIC_RANGES",
            "/// Code points for which `str.isnumeric()` is true — everything carrying a\n"
            "/// numeric value, including fractions and the numeric-letter categories.",
            numeric,
        ),
        range_table(
            "UNPRINTABLE_RANGES",
            "/// Code points for which `str.isprintable()` is FALSE: the separators (bar\n"
            "/// ASCII space), the control, format, surrogate, private-use and unassigned\n"
            "/// categories. `repr()` escapes exactly these.",
            unprintable,
        ),
    ]

    parts.append(
        range_table(
            "SPACE_RANGES",
            "/// Code points for which `str.isspace()` is true. Wider than Rust's\n"
            "/// `char::is_whitespace`, which follows the White_Space property alone and\n"
            "/// so misses the file/group/record/unit separators `\\x1c`-`\\x1f`.",
            space,
        )
    )

    rows = "\n".join(f"    (0x{cp:04X}, {esc(t)})," for cp, t in title)
    parts.append(
        "/// Titlecase mappings that differ from the uppercase one, sorted by key.\n"
        "/// Every other code point titlecases to its uppercase form.\n"
        "#[rustfmt::skip]\n"
        "pub(crate) static TITLECASE: &[(u32, &str)] = &[\n" + rows + "\n];\n"
    )

    OUT.write_text("\n".join(parts))
    print(
        f"wrote {OUT} — {len(decimal)}/{len(digit)}/{len(numeric)}/{len(unprintable)}"
        f"/{len(space)} ranges, {len(title)} titlecase entries"
    )


if __name__ == "__main__":
    main()
