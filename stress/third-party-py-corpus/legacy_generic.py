from typing import Generic, TypeVar

T = TypeVar("T")


class Box(Generic[T]):
    item: T


def unwrap(b: Box[int]) -> int:
    return b.item
