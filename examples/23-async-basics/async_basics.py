from __future__ import annotations
from typhon_runtime import Ok, Err, Result
import dataclasses
from typhon_runtime import Err as __typhon_Err__
import asyncio


@dataclasses.dataclass(slots=True)
class FetchError:
    url: str
    reason: str


async def fetch(url: str) -> Result[str, FetchError]:
    await asyncio.sleep(0.1)
    if "404" in url:
        return Err(FetchError(url=url, reason="not found"))
    return Ok(f"<body for {url}>")


async def fetch_and_size(url: str) -> Result[int, FetchError]:
    __typhon_q_0__ = await fetch(url)
    if isinstance(__typhon_q_0__, __typhon_Err__):
        return __typhon_q_0__
    body: str = __typhon_q_0__.value
    return Ok(len(body))


async def fetch_first_success(urls: list[str]) -> Result[str, FetchError]:
    last_err: FetchError | None = None
    for url in urls:
        match await fetch(url):
            case Ok(body):
                return Ok(body)
            case Err(err):
                last_err = err
    if last_err is None:
        return Err(FetchError(url="", reason="empty url list"))
    return Err(last_err)


async def main_async() -> None:
    match await fetch_and_size("https://example.com/page"):
        case Ok(n):
            print(f"size: {n}")
        case Err(e):
            print(f"err: {e.reason}")
    match await fetch_and_size("https://example.com/404"):
        case Ok(_):
            print("unexpected ok")
        case Err(e):
            print(f"expected err: {e.reason}")
    match await fetch_first_success(["https://a/404", "https://b/404", "https://c/ok"]):
        case Ok(body):
            print(f"got: {body}")
        case Err(e):
            print(f"all failed: {e}")


def main() -> None:
    asyncio.run(main_async())


if __name__ == "__main__":
    main()
