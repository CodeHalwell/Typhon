from __future__ import annotations
from typing import NewType

UserId = NewType("UserId", int)
PostId = NewType("PostId", int)
Email = NewType("Email", str)


def greet(uid: UserId, email: Email) -> str:
    return f"hi user#{uid} ({email})"


def double(n: int) -> int:
    return n * 2


def demo_construct_and_use() -> None:
    me: UserId = UserId(7)
    address: Email = Email("ada@example.com")
    print(greet(me, address))


def demo_escape_upward() -> None:
    me: UserId = UserId(21)
    twice: int = double(me)
    print(f"21 doubled = {twice}")


def demo_cross_newtype_rejected() -> None:
    pass


def main() -> None:
    demo_construct_and_use()
    demo_escape_upward()
    demo_cross_newtype_rejected()


if __name__ == "__main__":
    main()
