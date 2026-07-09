# tyc::perf_keys_membership

Advice-level diagnostic, **on by default** (part of the `tyc::perf_*` family,
gated by `[strictness] suggest-perf`). Fires on `EXPR in NAME.keys()` /
`EXPR not in NAME.keys()` — testing membership against a `.keys()` view when
the dict itself answers the same question directly. Safe anywhere, not just in
loops.

## Example

```ty
def has(config: dict[str, int], name: str) -> bool:
    return name in config.keys()     # advice: drop `.keys()`
```

## Why

`name in d` already tests membership against `d`'s keys — that's what `in` on a
dict means. `d.keys()` materialises a view object first for no benefit; the
direct form is shorter, clearer, and skips the view.

The diagnostic is **advice**, never an error, and the compiler never rewrites
the code.

## When it does *not* fire

- the membership is against the dict directly (`name in d`) — already optimal;
- `.keys()` is used for something other than a membership test (e.g.
  `for k in d.keys():`, or `list(d.keys())`, or a view set operation like
  `d1.keys() & d2.keys()`);
- the tested element is a bare constant literal (`"a" in d.keys()`) — a
  demonstration-shaped form; the lint targets testing a *variable* / expression
  against the keys.

## Fix

Drop `.keys()`:

```ty
def has(config: dict[str, int], name: str) -> bool:
    return name in config
```

Silence the whole family project-wide with `[strictness] suggest-perf = false`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/perf_keys_membership.md
