# tyc::empty_collection_no_annotation

Advisory lint (warning). Fires when an empty collection literal (`[]`,
`{}`, `set()`) is bound without a type annotation, so its element type
cannot be inferred and would silently become an implicit `Any`.

Typhon requires element types on collections (Rule 1 — no implicit
`Any`), and an empty literal gives the checker nothing to infer from.
Add the annotation at the binding site.

## Example

```ty
def main() -> None:
    # tyc::empty_collection_no_annotation — no element type to infer
    let xs = []

    # ✅ annotate the element type
    let ys: list[int] = []
    let counts: dict[str, int] = {}
```

## Fix

Write the collection type explicitly: `list[int]`, `dict[str, int]`,
`set[str]`, etc. Once the binding carries a type, later `.append(...)` /
insertion calls are checked against it.

## See also

- `tyc::missing_annotation` — the general no-implicit-`Any` rule.
