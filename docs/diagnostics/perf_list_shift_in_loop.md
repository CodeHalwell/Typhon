# tyc::perf_list_shift_in_loop

Advice-level diagnostic, **on by default** (part of the `tyc::perf_*` family,
gated by `[strictness] suggest-perf`). Fires when `LIST.insert(0, …)` or
`LIST.pop(0)` is called on a `list`-annotated binding inside a loop. Inserting
or removing at the *front* of a list is O(n) — every remaining element is
shifted one slot — so doing it once per iteration is O(n²).

## Example

```ty
def drain(queue: list[Task]) -> None:
    while queue:
        let task: Task = queue.pop(0)   # advice: O(n) front-shift each iteration
        handle(task)
```

## Why

A Python `list` is a contiguous array: `pop(0)` and `insert(0, …)` move every
element after the first. `collections.deque` is a doubly-linked ring buffer
with O(1) `popleft()` / `appendleft()`, purpose-built for FIFO / front-mutating
workloads.

The diagnostic is **advice**, never an error, and the compiler never rewrites
the code.

## When it does *not* fire

- the receiver isn't a bare `list`-annotated name (e.g. `self.items.pop(0)` —
  an attribute — is not flagged);
- the operation isn't at the front (`pop()` / `pop(-1)` / `append(...)` are all
  O(1) at the back);
- the call isn't inside a loop.

## Fix

Use a `collections.deque`:

```ty
import collections

def drain(tasks: list[Task]) -> None:
    let queue: collections.deque = collections.deque(tasks)
    while queue:
        let task: Task = queue.popleft()   # O(1)
        handle(task)
```

Silence the whole family project-wide with `[strictness] suggest-perf = false`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/perf_list_shift_in_loop.md
