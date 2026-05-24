from __future__ import annotations


def shift_char(c: str, k: int) -> str:
    if "a" <= c <= "z":
        return chr((ord(c) - ord("a") + k) % 26 + ord("a"))
    if "A" <= c <= "Z":
        return chr((ord(c) - ord("A") + k) % 26 + ord("A"))
    return c


def caesar_encrypt(text: str, k: int) -> str:
    return "".join((shift_char(c, k) for c in text))


def caesar_decrypt(text: str, k: int) -> str:
    return caesar_encrypt(text, -k)


def score_english(text: str) -> int:
    common: set[str] = {"the", "and", "of", "to", "in", "is", "a", "that"}
    return sum((1 for w in text.lower().split() if w in common))


def break_caesar(ciphertext: str) -> tuple[int, str]:
    best_k: int = 0
    best_score: int = -1
    best_plain: str = ciphertext
    for k in range(26):
        candidate: str = caesar_decrypt(ciphertext, k)
        s: int = score_english(candidate)
        if s > best_score:
            best_score = s
            best_k = k
            best_plain = candidate
    return (best_k, best_plain)


def main() -> None:
    plain: str = "The quick brown fox jumps over the lazy dog and is a happy animal"
    cipher: str = caesar_encrypt(plain, 7)
    print(f"plain:  {plain}")
    print(f"cipher: {cipher}")
    (k, recovered) = break_caesar(cipher)
    print(f"broken at k={k}: {recovered}")


if __name__ == "__main__":
    main()
