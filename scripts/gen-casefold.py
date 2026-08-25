#!/usr/bin/env python3
"""Regenerate the VM's embedded Unicode case-folding tables.

Emits ``tyc/crates/tyc-vm/src/casefold_data.rs`` from the hosting CPython's
own ``str.casefold()`` — the same operation the VM must match — so the VM's
``str.casefold()`` is byte-exact with the interpreter that runs the compiled
program. Rust's std has no case-folding, and neither ``to_lowercase`` nor an
uppercase round-trip reproduces it (dotless ``ı`` folds to itself but
round-trips through ``I`` to ``i``; Cherokee folds toward its uppercase forms,
the opposite of lowering), so the mappings are embedded directly.

Run from the repo root::

    python3 scripts/gen-casefold.py

Regenerate against the CPython version that hosts the target runtime; the
module header records the Unicode version captured. Case folding is very
stable across Unicode releases, so the skew for newer runtimes is confined to
a handful of exotic, newly added scalars.
"""

from __future__ import annotations

import unicodedata
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "tyc/crates/tyc-vm/src/casefold_data.rs"


def esc(cps: list[int]) -> str:
    """A Rust string literal for a fold target: ASCII printable literal,
    everything else an explicit ``\\u{..}`` escape (no invisible source)."""
    out = []
    for c in cps:
        if 0x20 <= c < 0x7F and c not in (0x22, 0x5C):  # not " or \
            out.append(chr(c))
        else:
            out.append("\\u{%X}" % c)
    return '"' + "".join(out) + '"'


def main() -> None:
    single: list[tuple[int, int]] = []
    multi: list[tuple[int, list[int]]] = []
    for cp in range(0, 0x110000):
        if 0xD800 <= cp <= 0xDFFF:  # surrogates are not scalar values
            continue
        ch = chr(cp)
        folded = ch.casefold()
        if folded == ch:
            continue
        cps = [ord(c) for c in folded]
        (single if len(cps) == 1 else multi).append(
            (cp, cps[0]) if len(cps) == 1 else (cp, cps)
        )
    single.sort()
    multi.sort()

    uni = unicodedata.unidata_version
    lines: list[str] = []
    lines.append("//! Unicode full case folding for `str.casefold()` (VM parity with CPython).")
    lines.append("//!")
    lines.append("//! Rust's std has no case-folding operation, and neither `to_lowercase` nor")
    lines.append("//! an uppercase-then-lowercase round-trip reproduces it: e.g. dotless `ı`")
    lines.append("//! must fold to itself but round-trips through `I` to `i`, and Cherokee folds")
    lines.append("//! *toward* its uppercase forms, the opposite of `to_lowercase`. So the C+F")
    lines.append("//! (common + full) mappings from the Unicode Character Database are embedded")
    lines.append("//! directly as the authority, giving byte-exact parity with `str.casefold()`.")
    lines.append("//!")
    lines.append("//! GENERATED FILE — do not edit by hand. Regenerate from the CPython that hosts")
    lines.append(f"//! the target runtime; captured here from Unicode {uni}. Every code point whose")
    lines.append("//! fold differs from itself appears in exactly one table; all others fold to")
    lines.append("//! themselves. See `scripts/gen-casefold.py` for the generator.")
    lines.append("")
    lines.append("/// 1:1 fold mappings (`code point` -> `folded code point`), sorted by key.")
    lines.append("#[rustfmt::skip]")
    lines.append("pub(crate) static CASEFOLD_SINGLE: &[(u32, u32)] = &[")
    row: list[str] = []
    for a, b in single:
        row.append(f"(0x{a:04X}, 0x{b:04X}),")
        if len(row) == 6:
            lines.append("    " + " ".join(row))
            row = []
    if row:
        lines.append("    " + " ".join(row))
    lines.append("];")
    lines.append("")
    lines.append("/// Full-fold expansions (`code point` -> `> 1` folded scalar values), sorted by key.")
    lines.append("#[rustfmt::skip]")
    lines.append("pub(crate) static CASEFOLD_MULTI: &[(u32, &str)] = &[")
    for a, cps in multi:
        lines.append(f"    (0x{a:04X}, {esc(cps)}),")
    lines.append("];")

    OUT.write_text("\n".join(lines) + "\n")
    print(f"wrote {OUT} — single={len(single)} multi={len(multi)} unicode={uni}")


if __name__ == "__main__":
    main()
