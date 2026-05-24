from __future__ import annotations
from typhon_runtime import Ok, Err, Result
import typhon_runtime
import dataclasses
import asyncio
import random


@dataclasses.dataclass(slots=True)
class User:
    id: int
    name: str


@dataclasses.dataclass(slots=True)
class Posts:
    items: list[str]


@dataclasses.dataclass(slots=True)
class Notifs:
    count: int


@dataclasses.dataclass(slots=True)
class Dashboard:
    user: User
    posts: Posts
    notifs: Notifs


async def fetch_user(uid: int) -> User:
    await asyncio.sleep(0.05)
    return User(id=uid, name=f"user-{uid}")


async def fetch_posts(uid: int) -> Posts:
    await asyncio.sleep(0.1)
    return Posts(items=[f"post-{uid}-{i}" for i in range(3)])


async def fetch_notifs(uid: int) -> Notifs:
    await asyncio.sleep(0.07)
    return Notifs(count=random.randint(0, 9))


async def load_dashboard(uid: int) -> Dashboard:
    async with asyncio.TaskGroup() as __typhon_tg_0__:
        __typhon_gather_1__ = __typhon_tg_0__.create_task(fetch_user(uid))
        __typhon_gather_2__ = __typhon_tg_0__.create_task(fetch_posts(uid))
        __typhon_gather_3__ = __typhon_tg_0__.create_task(fetch_notifs(uid))
    user = __typhon_gather_1__.result()
    posts = __typhon_gather_2__.result()
    notifs = __typhon_gather_3__.result()
    return Dashboard(user=user, posts=posts, notifs=notifs)


async def log_visit(uid: int) -> None:
    await asyncio.sleep(0.2)
    print(f"  [bg] logged visit for {uid}")


async def handle_request(uid: int) -> Dashboard:
    dash: Dashboard = await load_dashboard(uid)
    typhon_runtime.tasks.spawn(log_visit(uid))
    return dash


async def gather_best_effort(uids: list[int]) -> list[Result[Dashboard, str]]:
    tasks: list[asyncio.Task[Dashboard]] = [
        asyncio.create_task(load_dashboard(uid)) for uid in uids
    ]
    results = await asyncio.gather(*tasks, return_exceptions=True)
    wrapped: list[Result[Dashboard, str]] = []
    for r in results:
        if isinstance(r, BaseException):
            wrapped.append(Err(str(r)))
        else:
            wrapped.append(Ok(r))
    return wrapped


async def main_async() -> None:
    dash: Dashboard = await handle_request(42)
    print(
        f"loaded for {dash.user.name}: {len(dash.posts.items)} posts, {dash.notifs.count} notifs"
    )
    all_results: list[Result[Dashboard, str]] = await gather_best_effort([1, 2, 3])
    for r in all_results:
        match r:
            case Ok(d):
                print(f"  ok: {d.user.name}")
            case Err(e):
                print(f"  err: {e}")
    await asyncio.sleep(0.3)


def main() -> None:
    asyncio.run(main_async())


if __name__ == "__main__":
    main()
