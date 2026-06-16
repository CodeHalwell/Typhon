from __future__ import annotations
from typing import Iterator


def chunks[T](items: list[T], size: int) -> Iterator[list[T]]:
    i: int = 0
    while i < len(items):
        yield items[i : i + size]
        i = i + size


def take[T](it: Iterator[T], n: int) -> list[T]:
    result: list[T] = []
    count: int = 0
    for x in it:
        if count >= n:
            break
        result.append(x)
        count = count + 1
    return result


def main() -> None:
    data: list[int] = [1, 2, 3, 4, 5, 6, 7]
    for chunk in chunks(data, 3):
        print(chunk)
    print(take(chunks(data, 2), 2))


if __name__ == "__main__":
    main()
