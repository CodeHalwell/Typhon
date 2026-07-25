# tyc::implicit_any

Fires when a bare collection annotation (`list`, `dict`, `tuple`, `set`,
`frozenset`) appears without its element-type parameters. Per Typhon Rule 1
and `[strictness] no-implicit-any = true` (the default), every container
annotation should spell out its element types.

## Where it fires

**Annotated-assignment** positions — locals, module-level bindings, and
class-body field declarations:

```ty
def main() -> None:
    let xs: list = [1, 2, 3]        # error: bare `list` has an implicit Any element

CACHE: dict = {}                    # error: bare `dict`

class Bag:
    items: list                     # error: bare `list`
```

…and **function signature** positions — every parameter (positional,
keyword-only, `*args`, `**kwargs`) and the return annotation, on `def`,
`async def`, and methods in `impl` / `extend` blocks:

```ty
def keys(d: dict) -> list:          # two errors: parameter `d`, and the return type
    return list(d.keys())
```

The signature position is the higher-leverage one: a bare `dict` there widens
every call site *and* every use in the body to `Any`, which is precisely the
hole Rule 1 exists to close. The diagnostic's label names the offending slot
(``parameter `d` ``, `return type`) so a multi-slot signature reports one
diagnostic per bare head.

## Where it does *not* fire

- **Parameterised forms.** `list[int]`, `dict[str, int]`, `tuple[int, ...]`,
  `tuple[A, B]` — the element types are spelled out, which is the whole point.
- **Non-container heads and dotted names.** Only the five builtin container
  heads are implicated; `collections.OrderedDict` or a user class named `dict`
  reached through a module qualifier is not the builtin.
- **Compiler-synthesised helpers.** Anything named `__typhon_*` is exempt, as
  it is from Rule 1's `tyc::missing_annotation` — the desugarer's bridges are
  not the author's code to fix.
- **Lambdas**, which cannot carry annotations in Python at all, so there is no
  slot to check.
- **Third-party signatures recovered by venv introspection or a bundled
  stub.** Those become shapes, never source the checker walks, so a bare
  `dict` in a dependency is never blamed on your file.

It *does* fire inside an `unsafe:` region and inside a `.dty` stub, which is
deliberate and consistent with every other Rule 1 diagnostic (both
`tyc::missing_annotation` and the annotated-assignment half of this code
behave the same way). `unsafe:` relaxes *inferred* `Any` at an untyped
boundary; it is not a licence to *declare* one. If a signature genuinely
cannot name its element types, say so explicitly — `dict[str, object]`, or an
`as!` cast at the point of use.

## Why

`list` without a parameter implicitly means `list[Any]`, which silently
disables every meaningful type check that touches the value. The strict
default forces the author to record what's inside the container, which makes
both the call sites and the body type-checkable.

## Fix

Add element-type parameters that match what the code actually consumes and
produces:

```ty
def keys(d: dict[str, int]) -> list[str]:
    return list(d.keys())
```

## History

Signature coverage landed after the 2026-07-25 review (finding C2); before
that the check ran on annotated-assignment positions only. This is a
**deliberate narrowing** — it rejects programs that previously type-checked
*and ran correctly* — taken because Rule 1's guarantee is hollow if its
highest-leverage `Any` entry point is unchecked. Measured impact across
`examples/` and `stress/` at the time it landed: zero files affected beyond
the reproduction written for it.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/implicit_any.md
