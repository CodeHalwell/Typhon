# tyc::perf_str_concat_in_loop

Advice-level diagnostic, **on by default** (part of the `tyc::perf_*` family,
gated by `[strictness] suggest-perf`). Fires when a `str`-annotated (or `mut
NAME: str`) binding is grown with `NAME += …` inside a loop. Python strings are
immutable, so each `+=` allocates a fresh string and copies the whole
accumulator — quadratic over the loop.

## Example

```ty
def render(rows: list[str]) -> str:
    mut out: str = ""
    for row in rows:
        out += row + "\n"       # advice: quadratic string build
    return out
```

## Why

`out += row` can't grow `out` in place — it builds a new string of length
`len(out) + len(row)` and copies both in. Over `n` rows that's O(n²) total
copying. Collecting the pieces in a `list[str]` and joining once is O(n).

The diagnostic is **advice**, never an error, and the compiler never rewrites
the code.

## When it does *not* fire

- the target isn't a plain name (an attribute `self.buf += …` or subscript
  `parts[i] += …` is exempt);
- the accumulator isn't a `str` (`total: int += …` is fine);
- the `+=` isn't inside a loop.

## Fix

Collect parts and `"".join(...)` once after the loop:

```ty
def render(rows: list[str]) -> str:
    mut parts: list[str] = []
    for row in rows:
        parts.append(row + "\n")
    return "".join(parts)
```

Silence the whole family project-wide with `[strictness] suggest-perf = false`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/perf_str_concat_in_loop.md
