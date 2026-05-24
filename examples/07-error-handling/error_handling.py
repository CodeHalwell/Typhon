from __future__ import annotations
from typhon_runtime import Ok, Err, Result
import dataclasses
from typhon_runtime import Err as __typhon_Err__


@dataclasses.dataclass(slots=True)
class ParseError:
    field: str
    reason: str


def parse_port(raw: str) -> Result[int, ParseError]:
    if not raw.isdigit():
        return Err(ParseError(field="port", reason=f"not a number: {raw}"))
    n: int = int(raw)
    if n < 1 or n > 65535:
        return Err(ParseError(field="port", reason=f"out of range: {n}"))
    return Ok(n)


def parse_host(raw: str) -> Result[str, ParseError]:
    cleaned: str = raw.strip()
    if len(cleaned) == 0:
        return Err(ParseError(field="host", reason="empty"))
    return Ok(cleaned)


def parse_addr(host_raw: str, port_raw: str) -> Result[tuple[str, int], ParseError]:
    __typhon_with_0__ = parse_host(host_raw)
    if isinstance(__typhon_with_0__, __typhon_Err__):
        __typhon_with_err_0__ = __typhon_with_0__.error
        print(
            f"failed parsing {__typhon_with_err_0__.field}: {__typhon_with_err_0__.reason}"
        )
        return Err(__typhon_with_err_0__)
    host = __typhon_with_0__.value
    __typhon_with_1__ = parse_port(port_raw)
    if isinstance(__typhon_with_1__, __typhon_Err__):
        __typhon_with_err_1__ = __typhon_with_1__.error
        print(
            f"failed parsing {__typhon_with_err_1__.field}: {__typhon_with_err_1__.reason}"
        )
        return Err(__typhon_with_err_1__)
    port = __typhon_with_1__.value
    return Ok((host, port))


def parse_addr_short(
    host_raw: str, port_raw: str
) -> Result[tuple[str, int], ParseError]:
    __typhon_q_0__ = parse_host(host_raw)
    if isinstance(__typhon_q_0__, __typhon_Err__):
        return __typhon_q_0__
    host: str = __typhon_q_0__.value
    __typhon_q_1__ = parse_port(port_raw)
    if isinstance(__typhon_q_1__, __typhon_Err__):
        return __typhon_q_1__
    port: int = __typhon_q_1__.value
    return Ok((host, port))


def main() -> None:
    match parse_addr("localhost", "8080"):
        case Ok((host, port)):
            print(f"bound to {host}:{port}")
        case Err(e):
            print(f"failed: {e.reason}")
    match parse_addr("localhost", "70000"):
        case Ok(_):
            print("unexpected success")
        case Err(e):
            print(f"rejected: {e.field}={e.reason}")
    print(parse_addr_short(" example.com ", "443"))


if __name__ == "__main__":
    main()
