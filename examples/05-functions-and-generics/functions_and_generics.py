from __future__ import annotations
from typing import Callable


def add(a: int, b: int) -> int:
    return a + b


def greet(name: str, greeting: str = "Hello") -> str:
    return f"{greeting}, {name}!"


def stats(values: list[float]) -> tuple[float, float, float]:
    n: int = len(values)
    total: float = sum(values)
    mean: float = total / n
    lo: float = min(values)
    hi: float = max(values)
    return (mean, lo, hi)


def first[T](xs: list[T]) -> T | None:
    if len(xs) == 0:
        return None
    return xs[0]


def map_list[T, U](xs: list[T], f: Callable[[T], U]) -> list[U]:
    return [f(x) for x in xs]


def make_multiplier(factor: int) -> Callable[[int], int]:

    def inner(n: int) -> int:
        return n * factor

    return inner


def main() -> None:
    print(add(2, 3))
    print(greet("Ada"))
    print(greet("Ada", greeting="Howdy"))
    (mean, lo, hi) = stats([1.0, 2.0, 3.0, 4.0])
    print(f"mean={mean} min={lo} max={hi}")
    print(first([10, 20, 30]))
    print(first([]))
    print(map_list([1, 2, 3], lambda n: n * 10))
    times3: Callable[[int], int] = make_multiplier(3)
    print(times3(7))


if __name__ == "__main__":
    main()
