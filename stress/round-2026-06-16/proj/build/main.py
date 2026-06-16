from __future__ import annotations


def build_nested(depth: int) -> dict[str, object]:
    if depth == 0:
        return {"leaf": True}
    return {"level": depth, "child": build_nested(depth - 1)}


def count_depth(d: dict[str, object]) -> int:
    if "leaf" in d:
        return 0
    child = d["child"]
    if isinstance(child, dict):
        return 1 + count_depth(child)
    return 0


def main() -> None:
    nested: dict[str, object] = build_nested(5)
    print(count_depth(nested))


if __name__ == "__main__":
    main()
