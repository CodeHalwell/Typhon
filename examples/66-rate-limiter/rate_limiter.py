from __future__ import annotations
import dataclasses
import time


@dataclasses.dataclass(slots=True)
class TokenBucket:
    capacity: float
    refill_per_sec: float
    tokens: float = 0.0
    last_check: float = 0.0

    def consume(self, cost: float) -> bool:
        now: float = time.monotonic()
        if self.last_check == 0.0:
            self.last_check = now
            self.tokens = self.capacity
        elapsed: float = now - self.last_check
        self.tokens = min(self.capacity, self.tokens + elapsed * self.refill_per_sec)
        self.last_check = now
        if self.tokens >= cost:
            self.tokens = self.tokens - cost
            return True
        return False


def hammer(bucket: TokenBucket, attempts: int) -> tuple[int, int]:
    allowed: int = 0
    denied: int = 0
    for _ in range(attempts):
        if bucket.consume(1.0):
            allowed = allowed + 1
        else:
            denied = denied + 1
    return (allowed, denied)


def main() -> None:
    bucket: TokenBucket = TokenBucket(capacity=5.0, refill_per_sec=2.0)
    print("burst of 10:")
    (a, d) = hammer(bucket, 10)
    print(f"  allowed={a} denied={d}")
    print("waiting 1.0s for refill...")
    time.sleep(1.0)
    (a2, d2) = hammer(bucket, 5)
    print(f"  allowed={a2} denied={d2}")


if __name__ == "__main__":
    main()
