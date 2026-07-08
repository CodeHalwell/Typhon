# tyc::use_of_uninitialised

Fires when a `let NAME: T` binding declared without an initialiser
is read on a control-flow path where no preceding statement assigned
it. The companion to R3-8's relaxed declare-then-assign rule —
without this check the user would get a `NameError` at runtime
instead of a build-time diagnostic.

## Example

```ty
def bad(cond: bool) -> int:
    let x: int
    if cond:
        x = 5
    # else branch never assigns `x`
    return x   # ❌ tyc::use_of_uninitialised
```

## Why

R3-8 permits the natural declare-then-assign-in-arms shape:

```ty
def good(cond: bool) -> int:
    let x: int
    if cond:
        x = 5
    else:
        x = 10
    return x   # ✅ both branches assign `x`; DA pass intersects to {x}
```

The pass walks every function body once, tracking the
"definitely-assigned" set at each control-flow point. Sibling `if` /
`match` arms each contribute an assigned set; the post-branch state is
the **intersection** of the non-diverging arms (a `return` / `raise`
branch is excluded from the intersection because it never reaches the
join). Loops (`for` / `while`) intentionally do **not** propagate
assignments out — the body might execute zero times.

## Fix

Either initialise the binding at the declaration site:

```ty
let x: int = 0
```

Or ensure every path that reaches the read assigns the binding first:

```ty
let x: int
if cond:
    x = 5
else:
    x = 10       # add the missing arm
return x
```

For an early-exit error path, write the exit so it diverges (the DA
pass skips diverging branches when intersecting):

```ty
let x: int
match _load():
    case Ok(v):
        x = v
    case Err(e):
        return Err(e)   # diverges → excluded from intersection
return Ok(x)            # ✅ `x` is assigned on every non-diverging path
```

## Related

- **R3-8** documents the relaxed declare-then-assign rule that this
  check protects.
- **`tyc::immutable_assign`** fires on the SECOND assignment to a
  declare-only `let` binding; the first assignment is treated as the
  initialiser.
- **`tyc::non_exhaustive_match`** fires when a sealed-union `match`
  misses a variant; the DA pass trusts sealed-union exhaustiveness
  (the Result `case Ok(v): x = v / case Err(e): return Err(e)`
  pattern works without a `case _:`).

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/use_of_uninitialised.md
