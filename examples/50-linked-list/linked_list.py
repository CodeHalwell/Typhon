from __future__ import annotations
import dataclasses

type Node[T] = Cons[T] | Nil


@dataclasses.dataclass(slots=True)
class Cons[T]:
    head: T
    tail: Node[T]


@dataclasses.dataclass(slots=True)
class Nil:
    pass


def from_list[T](xs: list[T]) -> Node[T]:
    node: Node[T] = Nil()
    for i in range(len(xs) - 1, -1, -1):
        node = Cons(head=xs[i], tail=node)
    return node


def length[T](n: Node[T]) -> int:
    match n:
        case Cons(_, tail):
            return 1 + length(tail)
        case Nil():
            return 0


def to_list[T](n: Node[T]) -> list[T]:
    out: list[T] = []
    cur: Node[T] = n
    done: bool = False
    while not done:
        match cur:
            case Cons(head, tail):
                out.append(head)
                cur = tail
            case Nil():
                done = True
    return out


def reverse[T](n: Node[T]) -> Node[T]:
    acc: Node[T] = Nil()
    cur: Node[T] = n
    done: bool = False
    while not done:
        match cur:
            case Cons(head, tail):
                acc = Cons(head=head, tail=acc)
                cur = tail
            case Nil():
                done = True
    return acc


def main() -> None:
    xs: Node[int] = from_list([1, 2, 3, 4, 5])
    print("length:", length(xs))
    print("forward:", to_list(xs))
    print("reversed:", to_list(reverse(xs)))


if __name__ == "__main__":
    main()
