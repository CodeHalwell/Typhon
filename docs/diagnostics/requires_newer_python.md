# tyc::requires_newer_python

Fires when a file uses Python syntax that the project's `[python] target`
cannot parse: for example a Python 3.15 unpacking comprehension in a project
that targets 3.13.

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
  help: raise `[python] target` in typhon.toml to a version that has this
        syntax, or rewrite it without the newer form
```

## Why

`tyc` parses every file against the newest grammar it knows, so before this
check a 3.15-only construct type-checked on a 3.13 target, `tyc build` copied
it into the `.py`, and CPython 3.13 refused to compile the file. The program
could never have run on its target, so rejecting it at `tyc check` changes no
program that worked.

The syntax covered is whatever the vendored parser knows to be
version-gated. Above the 3.13 floor that is:

| Syntax | Needs |
|---|---|
| `except A, B:` without parentheses (PEP 758) | 3.14 |
| template strings, `t"..."` (PEP 750) | 3.14 |
| `*` / `**` unpacking in comprehensions, `[*xs for xs in lists]` (PEP 798) | 3.15 |

Typhon's own `lazy import ALIAS = MODULE` is not affected: it compiles on
every target, and only becomes the native PEP 810 statement in the emitted
Python of a 3.15 build.

## Severity

Error, in `tyc check`, `tyc build` and `tyc run`.

## Fix

Either raise the target to a version with the syntax:

```toml
[python]
target = "3.15"
```

or write it in a form the current target accepts:

```typhon
let flat: list[int] = [x for xs in lists for x in xs]
```
