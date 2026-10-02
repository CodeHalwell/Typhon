# tyc::nullable_use

Fires when a value of type `T | None` (a "nullable") is used where the
surrounding context demands `T`. Typhon writes the nullable shorthand `T?`
which means exactly the same thing as `T | None`.

## Example

```ty
def length_of(name: str?) -> int:
    return len(name)  # error: possibly-None value used where `str` is required
```

## Why

`None` is its own type. Calling `len(None)` or passing `None` to a function
that expects `str` raises at runtime, so the type checker forbids the use
until the value has been narrowed.

## Fix

Guard the value with an `is not None` check; the checker narrows the binding
inside the branch.

```ty
def length_of(name: str?) -> int:
    if name is not None:
        return len(name)
    return 0
```

## Nullable fields

The same rule applies when the possibly-`None` value is a *field* rather than a
local, and the guard narrows the whole path:

```ty
class Cfg:
    db: Db?

impl Cfg:
    def host(self) -> str:
        if self.db is None:
            return "localhost"
        return self.db.host  # ok — `self.db` is narrowed for the rest of the block
```

Without the guard, `self.db.host` reports `tyc::nullable_use`.

This field form landed in **v1.0.0-alpha.7** at **warn** level, because it had
never been checked before and an immediate error would have broken programs
whose nullable field happens always to be populated at the dereference. Since
the **2026-09-30 review** it is an **error by default** — Rule 3 (no implicit
`None`) is the language's headline guarantee, and a guarantee that only warns
is not one. Relax it during a migration:

```toml
[strictness]
nullable-use = "warn"   # or "off"
```

The bare-name form (`name.upper()` where `name: str?`) has always been, and
remains, an error and is not governed by the knob.

Narrowing follows `and` chains through attribute paths: in
`if b is not None and b.val is not None: b.val + 1` the second operand is
typed with the first already in force, so both `b` and `b.val` are narrowed in
the body (and after an early exit under the negated `or` form).

## Always-None receivers

A receiver that is not *possibly* `None` but *always* `None` — `None.attr`, or
a `-> None` function's result used as a receiver or operand — is the same
runtime failure, so it reports under the same code, but with wording that does
not suggest a guard (there is no non-`None` case to narrow to):

```ty
def notify() -> None:
    print("sent")

let n: int = notify().real   # error: `None` used where a non-None value is required
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/nullable_use.md

## Operands

An arithmetic operator or an *ordering* comparison (`<`, `<=`, `>`, `>=`)
rejects `None` at runtime, so a nullable operand is reported whatever shape
the expression has — a bare name, a call result, a subscript:

```ty
let counts: dict[str, int] = {"a": 1}
let total: int = counts.get("b") + 1   # error: `dict.get` is `int | None`
let small: bool = lookup("x") < 3      # error: `<` on a possibly-None value
```

Equality and identity (`==`, `!=`, `is`, `is not`) accept `None` on either side
and are not reported. An attribute-rooted operand (`self.count + 1`) reports
through the warn-level field form above, governed by `[strictness]
nullable-use`.
