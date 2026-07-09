# tyc::perf_sort_in_loop

Advice-level diagnostic, **on by default** (part of the `tyc::perf_*` family,
gated by `[strictness] suggest-perf`). Fires when `sorted(NAME)` or
`NAME.sort()` is called inside a loop and `NAME` is a bare name the loop never
content-mutates — so the same data is re-sorted (O(n log n)) on every
iteration.

## Example

```ty
def nearest_each(queries: list[Point], points: list[Point]) -> list[Point]:
    mut out: list[Point] = []
    for q in queries:
        let ordered: list[Point] = sorted(points)   # advice: `points` never changes here
        out.append(ordered[0])
    return out
```

## Why

If the input doesn't change inside the loop, the sort produces the same result
every time — hoisting it above the loop does the O(n log n) work once. When you
only need the smallest / largest few elements, `heapq.nsmallest` /
`heapq.nlargest` (or a running heap) avoids a full sort altogether.

The diagnostic is **advice**, never an error, and the compiler never rewrites
the code (reordering has observable effects the author must own).

## When it does *not* fire

Same conservatism as `tyc::perf_membership_in_loop`:

- the argument / receiver isn't a bare name;
- it *is* content-mutated in the loop (`append`, `pop`, reassignment, …), so
  re-sorting is genuinely needed (order-only ops like `.reverse()` don't count);
- it's the loop's own target variable (re-bound every iteration, so not
  invariant — `for g in groups: sorted(g)` is fine);
- the `sorted(...)` sits in a loop *header* (`for x in sorted(xs):` is
  idiomatic and runs once — not flagged).

## Fix

Hoist the sort, or reach for `heapq`:

```ty
def nearest_each(queries: list[Point], points: list[Point]) -> list[Point]:
    let ordered: list[Point] = sorted(points)   # sort once
    mut out: list[Point] = []
    for q in queries:
        out.append(ordered[0])
    return out
```

Silence the whole family project-wide with `[strictness] suggest-perf = false`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/perf_sort_in_loop.md
