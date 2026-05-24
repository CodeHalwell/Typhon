from __future__ import annotations
import functools
import math


@functools.cache
def fib(n: int) -> int:
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)


def hypot(a: float, b: float) -> float:
    return math.sqrt(a * a + b * b)


def clamp(x: float, lo: float, hi: float) -> float:
    if x < lo:
        return lo
    if x > hi:
        return hi
    return x


def mean(xs: list[float]) -> float:
    return sum(xs) / float(len(xs))


def variance(xs: list[float]) -> float:
    m: float = mean(xs)
    return sum(((x - m) * (x - m) for x in xs)) / float(len(xs))


def stddev(xs: list[float]) -> float:
    return math.sqrt(variance(xs))


def is_prime(n: int) -> bool:
    if n < 2:
        return False
    if n < 4:
        return True
    if n % 2 == 0:
        return False
    k: int = 3
    while k * k <= n:
        if n % k == 0:
            return False
        k = k + 2
    return True


def primes_up_to(n: int) -> list[int]:
    return [k for k in range(2, n + 1) if is_prime(k)]


def main() -> None:
    print([fib(i) for i in range(10)])
    print(hypot(3.0, 4.0))
    print(clamp(15.0, 0.0, 10.0))
    xs: list[float] = [4.0, 8.0, 15.0, 16.0, 23.0, 42.0]
    print(f"mean={mean(xs):.2f} stddev={stddev(xs):.2f}")
    print(primes_up_to(50))


if __name__ == "__main__":
    main()
