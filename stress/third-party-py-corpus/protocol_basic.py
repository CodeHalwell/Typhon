from typing import Protocol


class Drawable(Protocol):
    def draw(self) -> str:
        ...


class Square:
    side: int


def render(d: Drawable) -> str:
    return d.draw()
