from __future__ import annotations


def is_palindrome(s: str) -> bool:
    clean: str = "".join([c.lower() for c in s if c.isalnum()])
    return clean == clean[::-1]


def caesar(text: str, shift: int) -> str:
    result: str = ""
    for ch in text:
        if ch.isalpha():
            base: int = ord("a") if ch.islower() else ord("A")
            result = result + chr((ord(ch) - base + shift) % 26 + base)
        else:
            result = result + ch
    return result


def main() -> None:
    print(is_palindrome("A man a plan a canal Panama"))
    print(is_palindrome("hello"))
    enc: str = caesar("Hello, World!", 3)
    print(enc)
    print(caesar(enc, 23))


if __name__ == "__main__":
    main()
