# tyc::reserved_name

Fires when a `.ty` file uses a name that starts with `__typhon_` — as a
binding, a function, a parameter, an import alias or a plain reference.

That prefix belongs to the compiler. Every helper and temporary it emits lives
there: `__typhon_checked_cast__` (behind `as!`), `__typhon_freeze__` (behind
`freeze let`), `__typhon_ext_str__slug__` (an `extend str:` method),
`__typhon_unpack_0__`, `__typhon_qi_0__`, and so on.

## Example

```typhon
def __typhon_checked_cast__(value: object, tp: object) -> object:
    return value

let n: int = "not an int" as! int
```

```text
Error: tyc::reserved_name

  × `__typhon_checked_cast__` is in the `__typhon_` namespace, which is
  │ reserved for names the compiler generates
   ╭─[src/main.ty:1:5]
 1 │ def __typhon_checked_cast__(value: object, tp: object) -> object:
   ·     ───────────┬───────────
   ·                ╰── reserved prefix
   ╰────
  help: rename it — the compiler emits helpers and temporaries under this
        prefix, and a user binding of the same name silently replaces them
```

## Why

`EXPR as! TYPE` lowers to a call to `__typhon_checked_cast__`, which the build
imports from the generated runtime. A definition of the same name later in the
module wins, so the program above built without a diagnostic and the cast
checked nothing: `n` held a `str`. The same goes for any other generated name
— a collision either disables a runtime guarantee or changes what the lowered
code does, and nothing in the `.ty` source shows it.

The check reads the source text before any lowering, so it only ever sees
names the author wrote. Text inside string literals and comments is ignored.

## Fix

Rename the binding. No ordinary program needs a name in this namespace; the
generated helpers are reached through the surface syntax (`as!`, `freeze let`,
`extend`, `?`).

## Compatibility

Added in the 2026-11 security review. It rejects programs that built before,
which is deliberate: such a program was either replacing a compiler helper or
one rename away from doing so by accident.
