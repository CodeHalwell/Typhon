def first[T](xs: list[T]) -> T:
    return xs[0]


def doubled[N: int](x: N) -> int:
    return x * 2


def main() -> None:
    print(first([1, 2, 3]))
    print(doubled(7))
