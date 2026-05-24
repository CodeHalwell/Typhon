from __future__ import annotations
from typhon_runtime import Ok, Err, Result
import dataclasses
import argparse
import sys
from pathlib import Path

type Command = AddCmd | ListCmd | DoneCmd


@dataclasses.dataclass(slots=True)
class AddCmd:
    text: str


@dataclasses.dataclass(slots=True)
class ListCmd:
    show_done: bool


@dataclasses.dataclass(slots=True)
class DoneCmd:
    index: int


def parse_args(argv: list[str]) -> Result[Command, str]:
    parser = argparse.ArgumentParser(prog="todo", description="tiny todo list")
    subs = parser.add_subparsers(dest="cmd", required=True)
    add = subs.add_parser("add", help="add an item")
    add.add_argument("text", type=str)
    lst = subs.add_parser("list", help="list items")
    lst.add_argument("--all", action="store_true")
    done = subs.add_parser("done", help="mark item done")
    done.add_argument("index", type=int)
    try:
        ns = parser.parse_args(argv)
    except SystemExit:
        return Err("argparse exit")
    if ns.cmd == "add":
        return Ok(AddCmd(text=ns.text))
    if ns.cmd == "list":
        return Ok(ListCmd(show_done=ns.all))
    if ns.cmd == "done":
        return Ok(DoneCmd(index=ns.index))
    return Err(f"unknown command: {ns.cmd}")


STORE: Path = Path.home() / ".todo.txt"


def load_items() -> list[str]:
    if not STORE.exists():
        return []
    return [ln for ln in STORE.read_text(encoding="utf-8").splitlines() if len(ln) > 0]


def save_items(items: list[str]) -> None:
    STORE.write_text(
        """
""".join(items)
        + """
""",
        encoding="utf-8",
    )


def run(cmd: Command) -> int:
    items: list[str] = load_items()
    match cmd:
        case AddCmd(text):
            items.append(f"[ ] {text}")
            save_items(items)
            print(f"added: {text}")
            return 0
        case ListCmd(show_done):
            for i, item in enumerate(items):
                if not show_done and item.startswith("[x]"):
                    continue
                print(f"{i:3d}. {item}")
            return 0
        case DoneCmd(index):
            if index < 0 or index >= len(items):
                print(f"no such item: {index}")
                return 1
            items[index] = items[index].replace("[ ]", "[x]", 1)
            save_items(items)
            print(f"done: {items[index]}")
            return 0


def main() -> None:
    match parse_args(sys.argv[1:]):
        case Ok(cmd):
            sys.exit(run(cmd))
        case Err(_):
            sys.exit(2)


if __name__ == "__main__":
    main()
