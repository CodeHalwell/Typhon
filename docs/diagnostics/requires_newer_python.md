# tyc::requires_newer_python

Fires when a file uses Python syntax or a builtin that the project's
`[python] target` does not have: for example a Python 3.15 unpacking
comprehension, or `frozendict`, in a project that targets 3.13.

## Example

```typhon
# typhon.toml: [python] target = "3.13"
def main() -> None:
    let lists: list[list[int]] = [[1], [2, 3]]
    let flat: list[int] = [*xs for xs in lists]
    print(flat)
```

```text
Error: tyc::requires_newer_python

  × Cannot use iterable unpacking in a list comprehension on Python 3.13
  │ (syntax was added in Python 3.15)
   ╭─[src/main.ty:3:28]
 3 │     let flat: list[int] = [*xs for xs in lists]
   ·                            ─┬─
   ·                             ╰── needs a newer Python than the project targets
   ╰────
  help: raise `[python] target` in typhon.toml to a version that has it, or
        rewrite the code without it
```

## Why

`tyc` parses every file against the newest grammar it knows, so before this
check a 3.15-only construct type-checked on a 3.13 target, `tyc build` copied
it into the `.py`, and CPython 3.13 refused to compile the file. The program
could never have run on its target, so rejecting it at `tyc check` changes no
program that worked. The same holds for a builtin the target lacks: on 3.13,
`frozendict(...)` is a `NameError` (and was `tyc::unknown_name` before this
check existed).

The syntax covered is whatever the vendored parser knows to be
version-gated. Above the 3.13 floor that is:

| Feature | Needs |
|---|---|
| `except A, B:` without parentheses (PEP 758) | 3.14 |
| template strings, `t"..."` (PEP 750) | 3.14 |
| `*` / `**` unpacking in comprehensions, `[*xs for xs in lists]` (PEP 798) | 3.15 |
| the `frozendict` builtin (PEP 814) | 3.15 |
| the `sentinel` builtin (PEP 661) | 3.15 |

A module that binds `frozendict` or `sentinel` itself (its own class, a
function, an import, a parameter) is not checked for that name: there it may
well not be the builtin.

For a builtin the message reads:

```text
  × `frozendict` is a builtin added in Python 3.15 and the project targets
  │ 3.13
```

Typhon's own `lazy import ALIAS = MODULE` is not affected: it compiles on
every target, and only becomes the native PEP 810 statement in the emitted
Python of a 3.15 build.

## Severity

Error, in `tyc check`, `tyc build`, `tyc run` and the editor.

## Fix

Either raise the target to a version that has it:

```toml
[python]
target = "3.15"
```

or write it in a form the current target accepts:

```typhon
let flat: list[int] = [x for xs in lists for x in xs]
```
