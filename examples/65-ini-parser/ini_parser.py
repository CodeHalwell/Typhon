from __future__ import annotations
from typhon_runtime import Ok, Err, Result
import dataclasses


@dataclasses.dataclass(slots=True)
class ParseError:
    line: int
    msg: str


type Ini = dict[str, dict[str, str]]


def parse_ini(text: str) -> Result[Ini, ParseError]:
    sections: Ini = {}
    current: str = ""
    sections[current] = {}
    lineno: int = 0
    for raw in text.splitlines():
        lineno = lineno + 1
        line: str = raw.strip()
        if line == "" or line.startswith("#") or line.startswith(";"):
            continue
        if line.startswith("[") and line.endswith("]"):
            current = line[1:-1].strip()
            if current == "":
                return Err(ParseError(line=lineno, msg="empty section name"))
            if current not in sections:
                sections[current] = {}
            continue
        eq: int = line.find("=")
        if eq == -1:
            return Err(ParseError(line=lineno, msg=f"missing '=' in: {line}"))
        key: str = line[:eq].strip()
        value: str = line[eq + 1 :].strip()
        if key == "":
            return Err(ParseError(line=lineno, msg="empty key"))
        sections[current][key] = value
    return Ok(sections)


def render(ini: Ini) -> str:
    parts: list[str] = []
    for section, kv in ini.items():
        if section != "":
            parts.append(f"[{section}]")
        for k, v in kv.items():
            parts.append(f"{k} = {v}")
        parts.append("")
    return """
""".join(parts).rstrip()


def main() -> None:
    source: str = """# global settings
name = my-app

[server]
host = 0.0.0.0
port = 8080

; database config
[database]
url = postgres://localhost/db
pool_size = 10
"""
    match parse_ini(source):
        case Ok(ini):
            print("parsed sections:", list(ini.keys()))
            print()
            print(render(ini))
        case Err(e):
            print(f"line {e.line}: {e.msg}")


if __name__ == "__main__":
    main()
