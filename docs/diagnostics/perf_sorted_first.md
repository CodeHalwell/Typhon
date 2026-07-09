# tyc::perf_sorted_first

Advice-level diagnostic, **on by default** (part of the `tyc::perf_*` family,
gated by `[strictness] suggest-perf`). Fires on `sorted(EXPR)[0]` and
`sorted(EXPR)[-1]` — sorting an entire sequence (O(n log n)) just to read its
smallest or largest element, which `min(...)` / `max(...)` find in a single
O(n) pass.

## Example

```ty
def cheapest(items: list[Item]) -> Item:
    return sorted(items, )[0]        # advice: sorts everything to take one element
```

(Only the *bare* form — `sorted(...)` with no `key=` / `reverse=` — fires, so
the rewrite to `min` / `max` is unambiguous.)

## Why

`sorted(xs)[0]` orders all `n` elements and then discards all but one:
O(n log n) work for an O(n) answer. `min(xs)` / `max(xs)` scan once.

The diagnostic is **advice**, never an error, and the compiler never rewrites
the code.

## When it does *not* fire

- the `sorted(...)` call carries a `key=` or `reverse=` keyword (kept simple —
  only the bare form is flagged);
- the index isn't `[0]` or `[-1]` (a slice `[:3]` or a middle index really does
  need the full order).

## Fix

Use `min` / `max`:

```ty
def cheapest(items: list[Item]) -> Item:
    return min(items)
```

For a smallest / largest *few*, prefer `heapq.nsmallest(k, xs)` /
`heapq.nlargest(k, xs)` over a full sort-and-slice.

Silence the whole family project-wide with `[strictness] suggest-perf = false`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/perf_sorted_first.md
