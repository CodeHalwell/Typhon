from dataclasses import dataclass
from typing import Optional

@dataclass
class User:
    id: int
    name: str
    email: Optional[str] = None

def find_user(uid: int) -> Optional[User]:
    if uid == 0:
        return None
    return User(id=uid, name="amy", email="a@b.com")

COUNT: int = 10

def main() -> None:
    user = find_user(1)
    if user is not None:
        print(user.name)
    total = 0
    for i in range(COUNT):
        total = total + i
    print(total)

if __name__ == "__main__":
    main()
