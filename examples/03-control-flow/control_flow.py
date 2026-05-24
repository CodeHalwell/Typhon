from __future__ import annotations


def classify(score: int) -> str:
    if score >= 90:
        return "A"
    elif score >= 80:
        return "B"
    elif score >= 70:
        return "C"
    elif score >= 60:
        return "D"
    return "F"


def factorial(n: int) -> int:
    acc: int = 1
    i: int = 2
    while i <= n:
        acc = acc * i
        i = i + 1
    return acc


def sum_evens(xs: list[int]) -> int:
    total: int = 0
    for x in xs:
        if x % 2 != 0:
            continue
        total = total + x
    return total


def squares_up_to(n: int) -> list[int]:
    return [i * i for i in range(n)]


def word_lengths(words: list[str]) -> dict[str, int]:
    return {w: len(w) for w in words}


def main() -> None:
    print(classify(85))
    print(factorial(6))
    print(sum_evens([1, 2, 3, 4, 5, 6]))
    print(squares_up_to(5))
    print(word_lengths(["ant", "bee", "cat"]))


if __name__ == "__main__":
    main()
