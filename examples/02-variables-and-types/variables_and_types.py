from __future__ import annotations


def demo_primitives() -> None:
    answer: int = 42
    pi: float = 3.14159
    greeting: str = "hi"
    active: bool = True
    nothing: str | None = None
    print(answer, pi, greeting, active, nothing)


def demo_mutability() -> None:
    pi: float = 3.14159
    counter: int = 0
    counter = counter + 1
    counter = counter * 2
    print(f"pi={pi} counter={counter}")


def demo_nullable() -> None:
    maybe_name: str | None = lookup_name(1)
    if maybe_name is None:
        print("anonymous")
        return
    print(f"hi, {maybe_name}")


def lookup_name(id: int) -> str | None:
    if id == 1:
        return "Ada"
    return None


def demo_widening() -> None:
    n: int = 3
    x: float = n
    print(f"{n} widened to {x}")


def main() -> None:
    demo_primitives()
    demo_mutability()
    demo_nullable()
    demo_widening()


if __name__ == "__main__":
    main()
