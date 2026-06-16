from __future__ import annotations
import dataclasses

type JsonValue = JsonNull | JsonBool | JsonNum | JsonStr | JsonArr | JsonObj


@dataclasses.dataclass(slots=True, frozen=True)
class JsonNull:
    pass


@dataclasses.dataclass(slots=True, frozen=True)
class JsonBool:
    value: bool


@dataclasses.dataclass(slots=True, frozen=True)
class JsonNum:
    value: float


@dataclasses.dataclass(slots=True, frozen=True)
class JsonStr:
    value: str


@dataclasses.dataclass(slots=True, frozen=True)
class JsonArr:
    items: list[JsonValue]


@dataclasses.dataclass(slots=True, frozen=True)
class JsonObj:
    fields: list[tuple[str, JsonValue]]


def render(v: JsonValue) -> str:
    match v:
        case JsonNull():
            return "null"
        case JsonBool(value=b):
            return "true" if b else "false"
        case JsonNum(value=n):
            return str(n)
        case JsonStr(value=s):
            return f'"{s}"'
        case JsonArr(items=items):
            return "[" + ", ".join([render(it) for it in items]) + "]"
        case JsonObj(fields=fields):
            parts: list[str] = [f'"{k}": {render(val)}' for (k, val) in fields]
            return "{" + ", ".join(parts) + "}"


def main() -> None:
    doc: JsonValue = JsonObj(
        fields=[
            ("name", JsonStr(value="Alice")),
            ("age", JsonNum(value=30.0)),
            ("active", JsonBool(value=True)),
            ("tags", JsonArr(items=[JsonStr(value="a"), JsonStr(value="b")])),
        ]
    )
    print(render(doc))


if __name__ == "__main__":
    main()
