from __future__ import annotations
from typhon_runtime import try_result
from typhon_runtime import Ok, Err, Result
from typhon_runtime.cast import checked_cast as __typhon_checked_cast__
from typhon_runtime import Err as __typhon_Err__
import json


def parse_json(text: str) -> Result[dict[str, object], str]:
    return try_result(lambda: json.loads(text), lambda e: f"invalid JSON: {e}")


def read_service(text: str) -> Result[str, str]:
    __typhon_q_0__ = parse_json(text)
    if isinstance(__typhon_q_0__, __typhon_Err__):
        return __typhon_q_0__
    raw: dict[str, object] = __typhon_q_0__.value
    name: str = __typhon_checked_cast__(raw["name"], str)
    port: int = __typhon_checked_cast__(raw["port"], int)
    hosts: list[str] = __typhon_checked_cast__(raw["hosts"], list[str])
    limits: dict[str, int] = __typhon_checked_cast__(raw["limits"], dict[str, int])
    return Ok(describe(name, port, hosts, limits["rps"]))


def describe(name: str, port: int, hosts: list[str], rps: int) -> str:
    return f"{name} on :{port} across {hosts} @ {rps} rps"


def coerce_int(value: object) -> Result[int, str]:
    return try_result(
        lambda: __typhon_checked_cast__(value, int), lambda e: f"not an int: {e}"
    )


def main() -> None:
    good: str = (
        '{"name": "api", "port": 8080, "hosts": ["a", "b"], "limits": {"rps": 100}}'
    )
    match read_service(good):
        case Ok(summary):
            print(summary)
        case Err(msg):
            print(f"unexpected: {msg}")
    match read_service("not valid json"):
        case Ok(_):
            print("unexpected success")
        case Err(msg):
            print(f"rejected: {msg}")
    match coerce_int(42):
        case Ok(n):
            print(f"coerced int: {n}")
        case Err(msg):
            print(f"unexpected: {msg}")
    match coerce_int("oops"):
        case Ok(n):
            print(f"unexpected int: {n}")
        case Err(msg):
            print(f"caught bad cast: {msg}")


if __name__ == "__main__":
    main()
