from dataclasses import dataclass


@dataclass(frozen=True)
class Vec:
    x: int
    y: int


def magnitude(v: Vec) -> float:
    return (v.x * v.x + v.y * v.y) ** 0.5


def main() -> None:
    origin = Vec(x=0, y=0)
    print(magnitude(origin))
