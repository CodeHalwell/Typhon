# tyc::immutable_assign

Fires when a binding declared with `let` is re-assigned later in the same
scope. `let` is Typhon's immutable binding keyword (per Rule 2): once a name
is introduced with `let`, it points at one value for the rest of its lifetime.

## Example

```ty
def main() -> None:
    let count: int = 0
    count = count + 1  # error: cannot assign to immutable binding 'count'
```

## Why

Rule 2 of the Typhon language asks every local to spell out *whether it will
be rebound*. `let` says "no". This makes data flow visible at a glance: a
reader scanning the function knows that any `let` name on the left of `=` is
the original value, not a stale one.

## Fix

Change the declaration keyword to `mut`, which permits re-assignment of new
values of the same declared type:

```ty
def main() -> None:
    mut count: int = 0
    count = count + 1  # ok
```

## Inside a loop body

A `let` declared inside a `for` / `while` body is freshly bound on every
iteration, so a *later sibling loop* may reuse the same scratch name:

```ty
for a in xs:
    let t: int = a      # ok
for b in ys:
    let t: int = b      # ok — a different loop, a fresh binding
```

Reassigning that binding from inside the loop that declared it — or from a loop
nested within it — is still `tyc::immutable_assign`:

```ty
for a in xs:
    let t: int = a
    t = 99              # error: cannot assign to immutable binding 't'
```

This was not enforced before **v1.0.0-alpha.7**: the sibling-loop carve-out was
keyed on the declaration alone and could not tell the two cases apart.

## Through `global` / `nonlocal`

`global NAME` and `nonlocal NAME` name a binding in an outer scope. Assigning
through them is an assignment to *that* binding, so `let` applies:

```ty
let CONFIG: str = "a"

def go() -> None:
    global CONFIG
    CONFIG = "b"        # error: cannot assign to immutable binding 'CONFIG'
```

Declare it `mut` at module level if it is meant to be rebound. Before
**v1.0.0-alpha.7** this fired no diagnostic — the write silently created a
separate function-local binding instead, so the module constant was left alone
at check time and clobbered at run time.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/immutable_assign.md
