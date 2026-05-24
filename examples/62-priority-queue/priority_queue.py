from __future__ import annotations
from typhon_runtime import Ok, Err, Result
import dataclasses
import heapq


@dataclasses.dataclass(slots=True)
class Empty:
    pass


@dataclasses.dataclass(slots=True)
class PriorityQueue[T]:
    items: list[tuple[int, int, T]] = dataclasses.field(default_factory=list)
    counter: int = 0

    def push(self, priority: int, value: T) -> None:
        heapq.heappush(self.items, (priority, self.counter, value))
        self.counter = self.counter + 1

    def pop(self) -> Result[T, Empty]:
        if len(self.items) == 0:
            return Err(Empty())
        triple: tuple[int, int, T] = heapq.heappop(self.items)
        return Ok(triple[2])

    @property
    def size(self) -> int:
        return len(self.items)


@dataclasses.dataclass(slots=True)
class Task:
    name: str


def main() -> None:
    pq: PriorityQueue[Task] = PriorityQueue()
    pq.push(2, Task(name="write report"))
    pq.push(0, Task(name="fix outage"))
    pq.push(1, Task(name="reply to email"))
    pq.push(0, Task(name="call boss"))
    print("draining queue (lowest priority first):")
    while pq.size > 0:
        match pq.pop():
            case Ok(task):
                print(f"  -> {task.name}")
            case Err(_):
                pass


if __name__ == "__main__":
    main()
