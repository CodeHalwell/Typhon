from __future__ import annotations
import dataclasses

type State = Red | Yellow | Green


@dataclasses.dataclass(slots=True)
class Red:
    pass


@dataclasses.dataclass(slots=True)
class Yellow:
    pass


@dataclasses.dataclass(slots=True)
class Green:
    pass


def next_state(s: State) -> State:
    match s:
        case Red():
            return Green()
        case Green():
            return Yellow()
        case Yellow():
            return Red()


def name(s: State) -> str:
    match s:
        case Red():
            return "red"
        case Yellow():
            return "yellow"
        case Green():
            return "green"


def duration_seconds(s: State) -> int:
    match s:
        case Red():
            return 30
        case Yellow():
            return 5
        case Green():
            return 25


def cycle(start: State, steps: int) -> list[str]:
    out: list[str] = []
    cur: State = start
    for _ in range(steps):
        out.append(f"{name(cur)} ({duration_seconds(cur)}s)")
        cur = next_state(cur)
    return out


def main() -> None:
    for line in cycle(Red(), 6):
        print(line)


if __name__ == "__main__":
    main()
