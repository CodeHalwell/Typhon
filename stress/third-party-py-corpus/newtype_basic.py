from typing import NewType

UserId = NewType("UserId", int)
Email = NewType("Email", str)


def greet(uid: UserId, addr: Email) -> str:
    return f"hello {uid} at {addr}"
