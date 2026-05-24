from __future__ import annotations
from typhon_runtime import Ok, Err, Result
from typhon_runtime import Err as __typhon_Err__
from pathlib import Path


def write_lines(path: Path, lines: list[str]) -> Result[None, str]:
    try:
        path.write_text(
            """
""".join(lines)
            + """
""",
            encoding="utf-8",
        )
        return Ok(None)
    except OSError as e:
        return Err(f"could not write {path}: {e}")


def read_lines(path: Path) -> Result[list[str], str]:
    try:
        text: str = path.read_text(encoding="utf-8")
        return Ok([line for line in text.splitlines() if len(line) > 0])
    except FileNotFoundError:
        return Err(f"missing file: {path}")
    except OSError as e:
        return Err(f"could not read {path}: {e}")


def count_lines(path: Path) -> Result[int, str]:
    __typhon_q_0__ = read_lines(path)
    if isinstance(__typhon_q_0__, __typhon_Err__):
        return __typhon_q_0__
    lines: list[str] = __typhon_q_0__.value
    return Ok(len(lines))


def grep(path: Path, needle: str) -> Result[list[str], str]:
    __typhon_q_1__ = read_lines(path)
    if isinstance(__typhon_q_1__, __typhon_Err__):
        return __typhon_q_1__
    lines: list[str] = __typhon_q_1__.value
    return Ok([line for line in lines if needle in line])


def tail(path: Path, n: int) -> Result[list[str], str]:
    __typhon_q_2__ = read_lines(path)
    if isinstance(__typhon_q_2__, __typhon_Err__):
        return __typhon_q_2__
    lines: list[str] = __typhon_q_2__.value
    return Ok(lines[-n:])


def main() -> None:
    path: Path = Path("/tmp/typhon-text-demo.txt")
    match write_lines(path, ["alpha", "beta", "gamma", "delta", "epsilon"]):
        case Ok(_):
            print(f"wrote {path}")
        case Err(msg):
            print(msg)
            return
    match count_lines(path):
        case Ok(n):
            print(f"line count: {n}")
        case Err(msg):
            print(msg)
    match grep(path, "a"):
        case Ok(matches):
            print(f"lines containing 'a': {matches}")
        case Err(msg):
            print(msg)
    match tail(path, 2):
        case Ok(last):
            print(f"last 2 lines: {last}")
        case Err(msg):
            print(msg)


if __name__ == "__main__":
    main()
