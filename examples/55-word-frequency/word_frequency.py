from __future__ import annotations
import re
from collections import Counter


def tokenise(text: str) -> list[str]:
    return [w.lower() for w in re.findall("[A-Za-z']+", text)]


def top_n(text: str, n: int) -> list[tuple[str, int]]:
    counts: Counter[str] = Counter(tokenise(text))
    return counts.most_common(n)


def is_palindrome(word: str) -> bool:
    w: str = word.lower()
    return w == w[::-1]


def unique_palindromes(text: str) -> list[str]:
    words: set[str] = set(tokenise(text))
    return sorted((w for w in words if len(w) >= 3 and is_palindrome(w)))


def main() -> None:
    passage: str = "Mary had a little lamb, little lamb, little lamb. Mary had a little lamb whose fleece was white as snow. Madam, racecar, level, noon — a deed indeed."
    print("top 5:")
    for word, n in top_n(passage, 5):
        print(f"  {word:<8} {n}")
    print("palindromes:", unique_palindromes(passage))


if __name__ == "__main__":
    main()
