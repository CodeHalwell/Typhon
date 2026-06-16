from __future__ import annotations
import dataclasses
from abc import ABC, abstractmethod


class Shape(ABC):
    @abstractmethod
    def area(self) -> float: ...


@dataclasses.dataclass(slots=True)
class Circle(Shape):
    radius: float

    def area(self) -> float:
        return 3.14159 * self.radius * self.radius


@dataclasses.dataclass(slots=True)
class Square(Shape):
    side: float

    def area(self) -> float:
        return self.side * self.side


def main() -> None:
    shapes: list[Shape] = [Circle(radius=2.0), Square(side=3.0)]
    for s in shapes:
        print(round(s.area(), 2))


if __name__ == "__main__":
    main()
