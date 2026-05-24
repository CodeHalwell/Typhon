from __future__ import annotations


def demo_lists() -> None:
    scores: list[int] = [88, 92, 75, 60, 99]
    scores.append(100)
    scores.sort()
    top3: list[int] = scores[-3:]
    print(f"top three: {top3}")


def demo_dicts() -> None:
    prices: dict[str, float] = {"apple": 0.3, "banana": 0.15, "cherry": 2.5}
    for fruit, price in prices.items():
        print(f"{fruit:10s} ${price:.2f}")
    cherry_price: float | None = prices.get("cherry")
    if cherry_price is not None:
        print(f"cherry costs {cherry_price}")


def demo_tuples() -> None:
    point: tuple[float, float] = (3.0, 4.0)
    (x, y) = point
    print(f"distance from origin: {(x * x + y * y) ** 0.5}")


def demo_sets() -> None:
    a: set[int] = {1, 2, 3, 4}
    b: set[int] = {3, 4, 5, 6}
    print(f"intersection: {a & b}")
    print(f"union:        {a | b}")
    print(f"difference:   {a - b}")


def demo_slicing() -> None:
    xs: list[int] = [10, 20, 30, 40, 50, 60]
    print(xs[1:4])
    print(xs[::2])
    print(xs[::-1])


def main() -> None:
    demo_lists()
    demo_dicts()
    demo_tuples()
    demo_sets()
    demo_slicing()


if __name__ == "__main__":
    main()
