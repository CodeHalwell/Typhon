from __future__ import annotations
from typhon_runtime import Ok, Err, Result
import dataclasses
from collections import deque


@dataclasses.dataclass(slots=True)
class Empty:
    pass


@dataclasses.dataclass(slots=True)
class Stack[T]:
    items: list[T] = dataclasses.field(default_factory=list)

    def push(self, x: T) -> None:
        self.items.append(x)

    def pop(self) -> Result[T, Empty]:
        if len(self.items) == 0:
            return Err(Empty())
        return Ok(self.items.pop())

    def peek(self) -> Result[T, Empty]:
        if len(self.items) == 0:
            return Err(Empty())
        return Ok(self.items[-1])

    @property
    def size(self) -> int:
        return len(self.items)


@dataclasses.dataclass(slots=True)
class Queue[T]:
    items: deque[T]

    def enqueue(self, x: T) -> None:
        self.items.append(x)

    def dequeue(self) -> Result[T, Empty]:
        if len(self.items) == 0:
            return Err(Empty())
        return Ok(self.items.popleft())

    @property
    def size(self) -> int:
        return len(self.items)


def main() -> None:
    s: Stack[int] = Stack()
    s.push(1)
    s.push(2)
    s.push(3)
    print(f"stack size: {s.size}")
    match s.pop():
        case Ok(v):
            print(f"popped: {v}")
        case Err(_):
            print("stack underflow")
    q: Queue[str] = Queue(items=deque())
    q.enqueue("first")
    q.enqueue("second")
    q.enqueue("third")
    print(f"queue size: {q.size}")
    while q.size > 0:
        match q.dequeue():
            case Ok(item):
                print(f"  dequeued: {item}")
            case Err(_):
                pass


if __name__ == "__main__":
    main()
