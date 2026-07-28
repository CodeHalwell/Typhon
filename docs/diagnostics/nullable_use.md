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

Since **v1.0.0-alpha.7** this field form is reported at **warn** level rather
than error. It was never checked at all before that release, so making it an
error immediately would break programs whose nullable field happens always to
be populated at the dereference. Promote it once your code is clean:

```toml
[strictness]
nullable-use = "error"
```

It becomes an error by default in a later release. The bare-name form
(`name.upper()` where `name: str?`) has always been, and remains, an error.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/nullable_use.md
