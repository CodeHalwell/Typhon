from __future__ import annotations


def clean(raw: str) -> str:
    return str.replace(str.lower(str.strip(raw)), ",", "")


def normalise_username(raw: str | None) -> str:
    __typhon_mguard_0 = raw
    if __typhon_mguard_0 is None:
        return "anonymous"
    u = __typhon_mguard_0
    trimmed: str = u.strip()
    if len(trimmed) == 0:
        return "anonymous"
    return trimmed.lower()


def total_price(quantity: int | None, unit_price: float | None) -> float:
    __typhon_mguard_1 = quantity
    if __typhon_mguard_1 is None:
        return 0.0
    q = __typhon_mguard_1
    __typhon_mguard_2 = unit_price
    if __typhon_mguard_2 is None:
        return 0.0
    p = __typhon_mguard_2
    return float(q) * p


def fmt_words(words: list[str]) -> str:
    return ", ".join(sort_alpha(dedupe(filter_nonempty(words))))


def filter_nonempty(words: list[str]) -> list[str]:
    return [w for w in words if len(w) > 0]


def dedupe(words: list[str]) -> list[str]:
    seen: set[str] = set()
    result: list[str] = []
    for w in words:
        if w not in seen:
            seen.add(w)
            result.append(w)
    return result


def sort_alpha(words: list[str]) -> list[str]:
    return sorted(words)


def main() -> None:
    print(clean("  Hello, World  "))
    print(normalise_username(None))
    print(normalise_username("  AdaLovelace "))
    print(total_price(3, 4.99))
    print(total_price(None, 4.99))
    print(fmt_words(["zebra", "apple", "", "banana", "apple"]))


if __name__ == "__main__":
    main()
