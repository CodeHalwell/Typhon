# tyc::missing_initialiser

> **Status: currently unreachable.** The variant and its constructor still
> exist in `tyc-diagnostics`, but no pass in the compiler emits it today, so
> `tyc check` will never show it to you. It is documented here because
> `tyc explain missing_initialiser` still resolves, and because the rule it
> *used* to enforce is a common source of confusion. See
> [What replaced it](#what-replaced-it) for what actually fires now.

Originally fired when `let NAME: T` (or `mut NAME: T`) was written without an
`= <expr>` initialiser. Before v0.7.0, Typhon required every binding to carry
a value at its declaration and the Rust-style "declare now, assign later"
shape was rejected outright.

## What replaced it

**v0.7.0 added definite-assignment analysis and made declare-then-assign a
supported form.** A declare-only `let` is now legal; the resolver records the
declaration span and treats the *first* subsequent assignment as the
initialiser:

```ty
def parse(raw: str) -> Result[Cfg, str]:
    let loaded: Cfg               # ✅ legal since v0.7.0 — declare only
    match _load(raw):
        case Ok(v):  loaded = v   # this assignment IS the initialiser
        case Err(e): return Err(e)
    return Ok(loaded)
```

Two diagnostics police the form today:

- [`tyc::use_of_uninitialised`](./use_of_uninitialised.md) — the binding is
  *read* on a control-flow path that never assigned it. This is the one that
  catches real bugs.
- [`tyc::immutable_assign`](./immutable_assign.md) — a *second* assignment to a
  declare-only `let`. The first is the initialiser; anything after it violates
  immutability just as it would for `let x: int = 1`.

A declare-only `let` that is never assigned *and* never read is accepted
silently. It emits a bare `name: T` annotation in the generated Python, which
is inert at runtime — dead code rather than a bug.

## Fix

If you see this code in output from an older `tyc`, initialise the binding
inline:

```ty
def main() -> None:
    let x: int = 1
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/missing_initialiser.md
