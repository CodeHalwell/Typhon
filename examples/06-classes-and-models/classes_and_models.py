from __future__ import annotations
from pydantic import BaseModel, ConfigDict
import dataclasses


@dataclasses.dataclass(slots=True)
class User:
    id: int
    name: str
    email: str

    def display(self) -> str:
        return f"{self.name} <{self.email}> (#{self.id})"

    def domain(self) -> str:
        return self.email.split("@")[1]


@dataclasses.dataclass(slots=True, frozen=True)
class Point:
    x: float
    y: float

    def distance_to(self, other: Point) -> float:
        dx: float = self.x - other.x
        dy: float = self.y - other.y
        return (dx * dx + dy * dy) ** 0.5

    def translated(self, dx: float, dy: float) -> Point:
        return Point(x=self.x + dx, y=self.y + dy)


@dataclasses.dataclass(slots=True)
class Cart:
    items: list[str] = dataclasses.field(default_factory=list)

    @property
    def size(self) -> int:
        return len(self.items)

    def add(self, item: str) -> None:
        self.items.append(item)


class ApiUser(BaseModel):
    model_config = ConfigDict(extra="forbid")
    id: int
    name: str
    email: str
    age: int | None = None


def __typhon_ext_str__slug(self: str) -> str:
    return self.lower().replace(" ", "-")


def main() -> None:
    u: User = User(id=1, name="Ada Lovelace", email="ada@example.com")
    print(u.display())
    print(u.domain())
    origin: Point = Point(x=0.0, y=0.0)
    p: Point = Point(x=3.0, y=4.0)
    print(f"distance: {p.distance_to(origin)}")
    print(p.translated(1.0, 1.0))
    api: ApiUser = ApiUser(id=2, name="Grace Hopper", email="grace@example.com")
    print(api)
    title: str = "The Quick Brown Fox"
    print(__typhon_ext_str__slug(title))
    cart: Cart = Cart()
    cart.add("apple")
    cart.add("pear")
    print(f"cart size: {cart.size}")


if __name__ == "__main__":
    main()
