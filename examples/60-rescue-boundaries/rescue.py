from __future__ import annotations
from typhon_runtime import try_result
from typhon_runtime import Ok, Err, Result
import dataclasses
from typhon_runtime.cast import checked_cast as __typhon_checked_cast__
from typhon_runtime import Err as __typhon_Err__
import json

type ConfigError = NotFound | BadJson | BadField


@dataclasses.dataclass(slots=True)
class NotFound:
    path: str

    def display(self) -> str:
        match self:
            case NotFound(path):
                return f"not found: {path}"
            case BadJson(reason):
                return f"invalid JSON: {reason}"
            case BadField(field, reason):
                return f"bad {field}: {reason}"


@dataclasses.dataclass(slots=True)
class BadJson:
    reason: str

    def display(self) -> str:
        match self:
            case NotFound(path):
                return f"not found: {path}"
            case BadJson(reason):
                return f"invalid JSON: {reason}"
            case BadField(field, reason):
                return f"bad {field}: {reason}"


@dataclasses.dataclass(slots=True)
class BadField:
    field: str
    reason: str

    def display(self) -> str:
        match self:
            case NotFound(path):
                return f"not found: {path}"
            case BadJson(reason):
                return f"invalid JSON: {reason}"
            case BadField(field, reason):
                return f"bad {field}: {reason}"


@dataclasses.dataclass(slots=True)
class Config:
    host: str
    port: int


def parse_port(raw: str) -> Result[int, ConfigError]:
    __typhon_q_0__ = try_result(
        lambda: int(raw), lambda e: BadField(field="port", reason=str(e))
    )
    if isinstance(__typhon_q_0__, __typhon_Err__):
        return __typhon_q_0__
    n: int = __typhon_q_0__.value
    return Ok(n)


def load_config(text: str) -> Result[Config, ConfigError]:
    try:
        data: dict[str, str] = __typhon_checked_cast__(json.loads(text), dict[str, str])
        __typhon_q_1__ = parse_port(data["port"])
        if isinstance(__typhon_q_1__, __typhon_Err__):
            return __typhon_q_1__
        port: int = __typhon_q_1__.value
        return Ok(Config(host=data["host"], port=port))
    except Exception as e:
        return Err(BadJson(reason=str(e)))


def main() -> None:
    match load_config('{"host": "localhost", "port": "8080"}'):
        case Ok(cfg):
            print(f"ok {cfg.host}:{cfg.port}")
        case Err(err):
            print(f"error: {err.display()}")
    match load_config("not json"):
        case Ok(cfg):
            print(f"ok {cfg.host}:{cfg.port}")
        case Err(err):
            print(f"error: {err.display()}")
    match load_config('{"host": "localhost", "port": "x"}'):
        case Ok(cfg):
            print(f"ok {cfg.host}:{cfg.port}")
        case Err(err):
            print(f"error: {err.display()}")


if __name__ == "__main__":
    main()
