# 07-game-ecs friction notes

No major new friction encountered beyond the Round 1 findings in
`TYPHON_FEEDBACK.md`. The Round 1 issues I had to actively work around
while writing this app were:

- **#1 (variant-to-union upcast)** — every `Component` and `GameEvent`
  factory in `components.ty` / `events.ty` exists *only* to bridge a
  variant to its enclosing union across module boundaries. Without
  these I could not return a `GameEvent` from a system that wanted to
  emit a specific variant. Cost: ~13 factories for this app
  (7 Component + 6 GameEvent).
- **#2 (interfaces don't cross modules)** — the spec explicitly notes
  this. I designed `Scheduler` around `Callable[[World, float], None]`
  fields from the start instead of an `interface System: def run(...)`
  contract, exactly as Round 1 had to do for `Handler`.
- **#5 (exhaustive match still flags `missing_return`)** —
  `component_kind`, `event_label` both end with
  `raise RuntimeError("unreachable")` after the exhaustive match arms.
- **#4 (`T?` parameter mis-resolves cross-module)** — I avoided
  taking `Position?` / `Velocity?` etc. as function parameters
  entirely; the world's getters return them and callers narrow with
  `if x is not None` locally. No exposed `T?` parameters in this app.

Below are entries for friction points that felt *new* relative to
Round 1 — each is low or medium severity and could plausibly be a
documentation/checker tweak rather than a language change.

## 1. Wrapper-class boilerplate per sealed-union variant (severity: LOW)

Code that felt awkward:

```ty
# components.ty
pub class Position frozen:
    pos: Vec2

# But Position can't be a variant of `Component` directly,
# so I needed an indirection class:
pub class CompPosition:
    data: Position

pub type Component = CompPosition | CompVelocity | CompHealth | ...
```

Workaround applied (the pattern itself is the workaround):

```ty
pub class CompPosition: data: Position
pub class CompVelocity: data: Velocity
pub class CompHealth:   data: Health
pub class CompDamage:   data: Damage
pub class CompSprite:   data: Sprite
pub class CompCollider: data: Collider
pub class CompTag:      data: Tag
```

Why this is a weakness: any time the same underlying type (`Position`)
needs both standalone identity (it's stored in `World.positions:
dict[int, Position]`) *and* membership in a sum type (`Component`),
the user has to invent a one-field wrapper class purely for the type
algebra. The wrapper field name (`data`) is meaningless. A future
Typhon could either allow non-`pub` aliasing variants
(`pub variant CompPosition = Position`) or allow a class to be listed
directly as a variant of multiple unions without wrapping. In an ECS
this hits *every* component kind, so it doubles the number of class
declarations.

## 2. No keyword field patterns in `case` (re-encountered #9 — bites harder in wide records) (severity: LOW)

Code that felt awkward:

```ty
case EvHealthChanged(eid, old, new):
    return f"hp#{int(eid)}:{old}->{new}"
```

This is already noted in Round 1 #9 — but in this app I had matches
where I only cared about one field of a five-field event, and still
had to spell `_, _, _, _, value`. Once event classes start having more
than four fields it becomes order-fragile in a way Python's
`match X(field=name)` keyword patterns explicitly fix.

## 3. `mut` rebinding inside narrowing branch with a `let` default (severity: LOW)

Code that broke / felt awkward:

```ty
let h: Health? = world.get_health(eid)
let hp_str: str = "?"
if h is not None:
    hp_str = f"{h.hp}/{h.max_hp}"     # rebinds `let` — would fail
```

Workaround applied:

```ty
let h: Health? = world.get_health(eid)
mut hp_str: str = "?"
if h is not None:
    hp_str = f"{h.hp}/{h.max_hp}"
```

Why this is a weakness: idiomatic Python conditional-default is
`x = "?"; if cond: x = ...`. Typhon forces the author to *decide up
front* that the binding will be rebound, even when it's rebound in
exactly one branch. A `let`-with-later-assignment-in-conditional
inference (or sugar like `let x: str = "?" if h is None else
f"{h.hp}/{h.max_hp}"`) would remove the friction. This is small but
came up several times while writing `main.ty`.

---

Overall this app went together cleanly because the workarounds are now
well-understood; the bulk of the boilerplate is the factory and
wrapper-class layers required by Round 1 issues #1 and the
sealed-union-variant pattern.
