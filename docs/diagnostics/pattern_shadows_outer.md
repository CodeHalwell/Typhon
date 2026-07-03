# tyc::pattern_shadows_outer

Fires when a `case` pattern captures a name that already exists as an
immutable `let` in an enclosing scope. Python's `match` semantics make
pattern bindings real rebindings — the captured name is visible after
the `match` ends — so under Rule 2 it would trip `tyc::immutable_assign`
against the outer `let`. This diagnostic surfaces the same shape with
pattern-aware wording so the actionable advice is rename-the-capture,
not flip-the-outer-let to `mut`.

## Example

```ty
class Wrap:
    value: int

def main() -> None:
    let value: int = 99
    let b: Wrap = Wrap(value=5)
    match b:
        case Wrap(value):           # tyc::pattern_shadows_outer
            print(value)
```

## Why

Pattern captures in Rust/OCaml/Scala / Elixir / Haskell introduce
*fresh* bindings scoped to the arm — `match b { Wrap(value) => ... }`
shadows any outer `value` for the arm only. Python deliberately
diverges: `case Wrap(value):` rebinds the enclosing-scope name and
that rebinding outlives the `match`. Under Rule 2 a `let` binding
cannot be rebound, so the capture is a hard error.

The plain `tyc::immutable_assign` shape would suggest
`change \`let\` to \`mut\`` — but that's the wrong advice here. The
user almost certainly wants a fresh binding (the Rust/OCaml shape),
not to overwrite the outer one.

## Fix

Pick a fresh name for the capture (the convention used elsewhere in
the codebase: a short suffix on the pattern variable):

```ty
match b:
    case Wrap(inner):
        print(inner)
```

If you genuinely *do* want the pattern to overwrite the outer name —
the unusual case — change the outer declaration to `mut`:

```ty
mut value: int = 99
match b:
    case Wrap(value):       # now allowed
        print(value)
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/pattern_shadows_outer.md
