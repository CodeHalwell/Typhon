from __future__ import annotations
import enum


class Direction(enum.Enum):
    NORTH = enum.auto()
    EAST = enum.auto()
    SOUTH = enum.auto()
    WEST = enum.auto()


class Priority(enum.Enum):
    LOW = 10
    MEDIUM = 20
    HIGH = 30
    URGENT = enum.auto()


def turn_right(d: Direction) -> Direction:
    match d:
        case Direction.NORTH:
            return Direction.EAST
        case Direction.EAST:
            return Direction.SOUTH
        case Direction.SOUTH:
            return Direction.WEST
        case Direction.WEST:
            return Direction.NORTH
    raise RuntimeError("unreachable")


def sla_minutes(p: Priority) -> int:
    match p:
        case Priority.LOW:
            return 24 * 60
        case Priority.MEDIUM:
            return 4 * 60
        case Priority.HIGH:
            return 60
        case Priority.URGENT:
            return 15
    raise RuntimeError("unreachable")


def main() -> None:
    facing: Direction = Direction.NORTH
    steps: list[str] = []
    for _ in range(4):
        steps.append(facing.name)
        facing = turn_right(facing)
    print("clockwise: " + " -> ".join(steps))
    for p in Priority:
        print(f"{p.name} (value={p.value}) -> SLA {sla_minutes(p)} min")
    recovered: Priority = Priority(30)
    print(f"Priority(30) is {recovered.name}")


if __name__ == "__main__":
    main()
