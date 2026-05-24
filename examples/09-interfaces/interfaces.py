from __future__ import annotations
from typing import Protocol
import dataclasses


class Drawable(Protocol):
    def draw(self) -> None: ...

    def width(self) -> float: ...


class Serialisable(Protocol):
    def to_json(self) -> str: ...


@dataclasses.dataclass(slots=True)
class Button:
    label: str

    def draw(self) -> None:
        print(f"[ {self.label} ]")

    def width(self) -> float:
        return float(len(self.label) + 4)

    def to_json(self) -> str:
        return f'{{"type": "button", "label": "{self.label}"}}'


@dataclasses.dataclass(slots=True)
class Slider:
    value: float
    max: float

    def draw(self) -> None:
        filled: int = int(20.0 * self.value / self.max)
        empty: int = 20 - filled
        print("[" + "#" * filled + "-" * empty + "]")

    def width(self) -> float:
        return 22.0

    def to_json(self) -> str:
        return f'{{"type": "slider", "value": {self.value}, "max": {self.max}}}'


def render(items: list[Drawable]) -> None:
    for item in items:
        item.draw()


def serialise_all(items: list[Serialisable]) -> list[str]:
    return [item.to_json() for item in items]


def main() -> None:
    widgets: list[Drawable] = [
        Button(label="OK"),
        Button(label="Cancel"),
        Slider(value=7.0, max=10.0),
    ]
    render(widgets)
    json_items: list[Serialisable] = [Button(label="Save"), Slider(value=3.0, max=10.0)]
    for line in serialise_all(json_items):
        print(line)


if __name__ == "__main__":
    main()
