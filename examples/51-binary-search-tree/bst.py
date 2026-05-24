from __future__ import annotations
import dataclasses

type Tree = Branch | Leaf


@dataclasses.dataclass(slots=True)
class Branch:
    value: int
    left: Tree
    right: Tree


@dataclasses.dataclass(slots=True)
class Leaf:
    pass


def insert(t: Tree, v: int) -> Tree:
    match t:
        case Leaf():
            return Branch(value=v, left=Leaf(), right=Leaf())
        case Branch(value, left, right):
            if v < value:
                return Branch(value=value, left=insert(left, v), right=right)
            if v > value:
                return Branch(value=value, left=left, right=insert(right, v))
            return t


def contains(t: Tree, v: int) -> bool:
    match t:
        case Leaf():
            return False
        case Branch(value, left, right):
            if v == value:
                return True
            if v < value:
                return contains(left, v)
            return contains(right, v)


def in_order(t: Tree) -> list[int]:
    match t:
        case Leaf():
            return []
        case Branch(value, left, right):
            return in_order(left) + [value] + in_order(right)


def depth(t: Tree) -> int:
    match t:
        case Leaf():
            return 0
        case Branch(_, left, right):
            l: int = depth(left)
            r: int = depth(right)
            return 1 + (l if l > r else r)


def from_values(xs: list[int]) -> Tree:
    t: Tree = Leaf()
    for v in xs:
        t = insert(t, v)
    return t


def main() -> None:
    t: Tree = from_values([5, 2, 8, 1, 3, 7, 9, 4])
    print("in_order:", in_order(t))
    print("contains 7:", contains(t, 7))
    print("contains 6:", contains(t, 6))
    print("depth:", depth(t))


if __name__ == "__main__":
    main()
