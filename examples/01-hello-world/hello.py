from __future__ import annotations
import sys


def main() -> None:
    name: str = sys.argv[1] if len(sys.argv) > 1 else "world"
    print(f"Hello, {name}!")


if __name__ == "__main__":
    main()
