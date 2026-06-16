from __future__ import annotations
import dataclasses


@dataclasses.dataclass(slots=True)
class Logger:
    prefix: str | None

    def log(self, msg: str) -> str:
        if self.prefix is None:
            return msg
        return f"[{self.prefix.upper()}] {msg}"


def main() -> None:
    print(Logger(prefix="info").log("hello"))
    print(Logger(prefix=None).log("world"))


if __name__ == "__main__":
    main()
