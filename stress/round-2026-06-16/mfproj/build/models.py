from __future__ import annotations
from typing import NewType

__all__ = ["UserId", "User", "Event", "Created", "Updated", "Deleted"]
import dataclasses

UserId = NewType("UserId", int)


@dataclasses.dataclass(slots=True)
class User:
    id: UserId
    name: str
    email: str | None


type Event = Created | Updated | Deleted


@dataclasses.dataclass(slots=True, frozen=True)
class Created:
    user: User


@dataclasses.dataclass(slots=True, frozen=True)
class Updated:
    user: User
    field: str


@dataclasses.dataclass(slots=True, frozen=True)
class Deleted:
    user_id: UserId
