from __future__ import annotations
from .models import User, UserId, Event, Created, Updated, Deleted
from .repo import Repo


def describe(e: Event) -> str:
    match e:
        case Created(user=u):
            return f"created {u.name}"
        case Updated(user=u, field=f):
            return f"updated {u.name}.{f}"
        case Deleted(user_id=uid):
            return f"deleted user {int(uid)}"


def main() -> None:
    repo: Repo = Repo(users={}, events=[])
    repo.add(User(id=UserId(1), name="Alice", email="a@b.com"))
    repo.add(User(id=UserId(2), name="Bob", email=None))
    found: User | None = repo.get(UserId(1))
    if found is not None:
        print(found.name)
    repo.remove(UserId(2))
    for e in repo.events:
        print(describe(e))


if __name__ == "__main__":
    main()
