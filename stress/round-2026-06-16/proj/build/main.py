from __future__ import annotations


def analyze(xs: list[int]) -> str:
    match xs:
        case []:
            return "empty"
        case [x]:
            return f"single: {x}"
        case [x, y]:
            return f"pair: {x},{y}"
        case [first, *middle, last]:
            return f"first={first}, last={last}, mid={len(middle)}"


def main() -> None:
    print(analyze([]))
    print(analyze([1]))
    print(analyze([1, 2]))
    print(analyze([1, 2, 3, 4, 5]))


if __name__ == "__main__":
    main()
