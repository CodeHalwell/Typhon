# tyc::missing_field_init

Fires when an instance constructed via `X.__new__(X)` or
`object.__new__(X)` (bypassing the auto-generated `__init__`) escapes
the function — returned, passed as an argument — without every
required field having been assigned.

## Example

```ty
class ApiClient:
    api_key: str
    base_url: str

def make() -> ApiClient:
    let c: ApiClient = ApiClient.__new__(ApiClient)
    c.base_url = "https://api.example.com"
    return c
    # error: instance of `ApiClient` escapes without all required
    # fields set; missing: api_key
```

Without this audit the emitted Python would crash with
`AttributeError: 'ApiClient' object has no attribute 'api_key'` the
first time the missing field is read.

## Fix

Either assign every required field before the instance escapes, or —
preferably — use the normal constructor, which enforces field
initialisation at compile time:

```ty
def make() -> ApiClient:
    let c: ApiClient = ApiClient(api_key="…", base_url="https://api.example.com")
    return c
```

The normal constructor call is also checked by `tyc::arg_count`, so a
forgotten field is flagged at the *call site* (the most useful place)
rather than at the escape site.

## What's tracked

Only two construction shapes engage the audit:

- `<ClassName>.__new__(<ClassName>)`
- `object.__new__(<ClassName>)`

Tracking is dropped (conservatively, to avoid false positives) when
any of the following happen before the escape:

- `setattr(c, ...)` — dynamic attribute assignment defeats static
  field tracking.
- `c.method(...)` — a method call may initialise fields internally,
  so the audit can't be sure they're still missing.
- The binding is reassigned to anything other than another bypass
  call.
- The binding's enclosing scope was wrapped in `unsafe:` — the user
  opted out of the static-type discipline.

## Escapes covered

- `return c` from a function with required fields unassigned.
- `c` as a call argument to any function (`do_something(c)`).
- `let xs: list[Cls] = [c]` — container-literal escape via an
  annotated assignment. Generalises to dict / tuple / set literals
  and to any RHS that mentions the partial binding.
- `let alias: Cls = c` — outer-scope assignment escape via an
  annotated binding.

The audit fires only on annotated assignments (`Stmt::AnnAssign`)
because the explicit type commitment is what marks the value as
"leaving construction." A bare `c = something` is treated as
intra-function mutation, not an escape.

## Limitations

- The audit is intra-procedural: a helper method (`c.configure()`)
  that genuinely assigns required fields will (correctly) suppress
  the diagnostic, but a helper that doesn't will also (incorrectly)
  suppress it. Cross-function tracking would need a summary IR.
- Subclass field requirements are not tracked separately; the audit
  uses the fields declared on the class named in
  `<ClassName>.__new__(<ClassName>)`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/missing_field_init.md
