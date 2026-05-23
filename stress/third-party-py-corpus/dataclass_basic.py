from dataclasses import dataclass
from typing import Optional


@dataclass
class User:
    id: int
    name: str
    email: Optional[str] = None


def find(uid: int) -> Optional[User]:
    if uid <= 0:
        return None
    return User(id=uid, name="amy")


def main() -> None:
    u = find(1)
    if u is not None:
        print(u.name)
