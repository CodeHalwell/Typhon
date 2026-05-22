# tyc::duplicate_method

Fires when two `impl` / `extend` blocks (or one of each) on the same
class both define a method with the same name. The desugar pass merges
methods from every `impl ClassName:` and `extend ClassName:` block into
the target class's body — without a duplicate check, two `def get(self)`
methods would end up in the merged Python output, where Python takes
the last one and one definition is lost silently.

## Example

```ty
class Box:
    value: int

impl Box:
    def get(self) -> int:
        return self.value

extend Box:
    # tyc::duplicate_method — `get` is defined more than once on `Box`
    def get(self) -> int:
        return self.value * 2
```

## Why

`impl`/`extend` are designed to compose: multiple blocks for the same
class are merged at desugar so larger projects can split a class's
method surface across files without inheritance. The natural expectation
is that the merge is a *union*, not a *replacement* — and so a
duplicate method name is almost always a copy-paste mistake or a
disagreement between two contributors. Surfacing it at compile time
prevents the silent override.

## Fix

Pick one of:

- Rename the second method (`get_doubled`, `get_raw`, etc.).
- Delete the duplicate definition if it was a copy-paste.
- Merge the two bodies into a single `impl` / `extend` block.

The diagnostic span anchors the *second* `def`, so the first definition
is the one preserved on a quick `git revert` of the duplicate file.

## Notes

The check fires after the type checker's class-shape merge, so a
spurious `__typhon_impl_X` from an `impl UnknownClass:` does not double-
report alongside `tyc::impl_unknown_class`.
