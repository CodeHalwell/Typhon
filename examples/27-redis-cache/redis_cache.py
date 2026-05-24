from __future__ import annotations
import dataclasses
import json
import time
from typing import Callable
import redis


@dataclasses.dataclass(slots=True)
class CacheError:
    op: str
    reason: str


def open_redis(url: str = "redis://localhost:6379/0") -> redis.Redis:
    return redis.Redis.from_url(url, decode_responses=True)


def cached[T](
    client: redis.Redis,
    key: str,
    ttl_seconds: int,
    compute: Callable[[], T],
    serialise: Callable[[T], str],
    deserialise: Callable[[str], T],
) -> T:
    cached_raw: str | None = client.get(key)
    if cached_raw is not None:
        return deserialise(cached_raw)
    fresh: T = compute()
    client.setex(key, ttl_seconds, serialise(fresh))
    return fresh


def expensive_query(user_id: int) -> dict[str, object]:
    print(f"  [miss] running expensive query for {user_id}")
    time.sleep(0.2)
    return {"user_id": user_id, "score": user_id * 17, "tier": "gold"}


def increment_counter(client: redis.Redis, key: str) -> int:
    return int(client.incr(key))


def track_event(client: redis.Redis, user_id: int, event: str) -> None:
    pipe = client.pipeline(transaction=False)
    pipe.hincrby(f"user:{user_id}:events", event, 1)
    pipe.expire(f"user:{user_id}:events", 86400)
    pipe.zadd("user:active", {str(user_id): time.time()})
    pipe.execute()


def main() -> None:
    client = open_redis()
    try:
        client.ping()
    except redis.RedisError as e:
        print(f"redis unavailable: {e}")
        return
    key: str = "user:42:profile"
    for _ in range(3):
        profile: dict[str, object] = cached(
            client,
            key,
            ttl_seconds=10,
            compute=lambda: expensive_query(42),
            serialise=json.dumps,
            deserialise=json.loads,
        )
        print(f"  profile: {profile}")
    hits: int = increment_counter(client, "hits:home")
    print(f"home hits: {hits}")
    track_event(client, 42, "view")
    track_event(client, 42, "click")
    print(f"events: {client.hgetall('user:42:events')}")


if __name__ == "__main__":
    main()
