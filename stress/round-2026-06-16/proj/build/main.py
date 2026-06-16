from __future__ import annotations
import dataclasses
from typing import Iterator


@dataclasses.dataclass(slots=True)
class CountDown:
    start: int

    def __iter__(self) -> Iterator[int]:
        return self

    def __next__(self) -> int:
        if self.start <= 0:
            raise StopIteration()
        self.start = self.start - 1
        return self.start + 1


def main() -> None:
    result: list[int] = []
    cd: CountDown = CountDown(start=5)
    for n in cd:
        result.append(n)
    print(result)


if __name__ == "__main__":
    main()
