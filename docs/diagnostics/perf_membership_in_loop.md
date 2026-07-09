# tyc::perf_membership_in_loop

Advice-level diagnostic, **on by default** (part of the `tyc::perf_*` family,
gated by `[strictness] suggest-perf`). Fires when an `x in NAME` / `x not in
NAME` test appears in an `if` / `while` condition inside a `for` / `while`
loop, where `NAME` is a `list`-annotated (or list-literal) binding that the
loop never mutates. Each iteration re-scans the whole list — O(n) per test,
O(n·m) over the loop.

## Example

```ty
def count_allowed(events: list[Event], allowed: list[str]) -> int:
    mut hits: int = 0
    for e in events:
        if e.kind in allowed:        # advice: linear scan of `allowed` each iteration
            hits += 1
    return hits
```

## Why

Membership on a `list` is a linear scan. When the list is loop-invariant, the
scan is pure repeated work: building a `set` once before the loop turns each
test into an O(1) hash lookup, dropping the loop from O(n·m) to O(n).

The diagnostic is **advice**, never an error, and the compiler never rewrites
the code — a `set` doesn't preserve order or duplicates, so the substitution is
the author's call.

## When it does *not* fire

Conservative by design — it stays silent unless the win is unambiguous:

- the container isn't provably a `list` (an unannotated parameter, or a
  `dict` / `set` / `str`, where `in` is already O(1) or a different operation);
- the container is mutated inside the loop (`append`, `insert`, `remove`,
  `pop`, reassignment, `x[i] = …`, `del`), so it isn't loop-invariant;
- the test isn't inside a loop, or isn't in an `if` / `while` condition.

## Fix

Hoist a `set` out of the loop:

```ty
def count_allowed(events: list[Event], allowed: list[str]) -> int:
    let allowed_set: set[str] = set(allowed)
    mut hits: int = 0
    for e in events:
        if e.kind in allowed_set:    # O(1) membership
            hits += 1
    return hits
```

Silence the whole family project-wide with `[strictness] suggest-perf = false`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/perf_membership_in_loop.md
