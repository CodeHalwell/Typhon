from __future__ import annotations

__all__ = ["Repo"]
import dataclasses
from .models import User, UserId, Event, Created, Deleted


@dataclasses.dataclass(slots=True)
class Repo:
    users: dict[int, User]
    events: list[Event]

    def add(self, user: User) -> None:
        self.users[int(user.id)] = user
        self.events.append(Created(user=user))

    def get(self, uid: UserId) -> User | None:
        return self.users.get(int(uid))

    def remove(self, uid: UserId) -> bool:
        if int(uid) in self.users:
            del self.users[int(uid)]
            self.events.append(Deleted(user_id=uid))
            return True
        return False
