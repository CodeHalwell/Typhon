from __future__ import annotations
from pydantic import BaseModel, ConfigDict
from typhon_runtime import Ok, Err, Result
import dataclasses
import time
import requests


class Repo(BaseModel):
    model_config = ConfigDict(extra="forbid")
    full_name: str
    description: str | None
    stargazers_count: int
    language: str | None
    html_url: str


@dataclasses.dataclass(slots=True)
class HttpError:
    status: int | None
    message: str


def fetch_repo(owner: str, name: str) -> Result[Repo, HttpError]:
    url: str = f"https://api.github.com/repos/{owner}/{name}"
    try:
        resp: requests.Response = requests.get(url, timeout=10.0)
    except requests.RequestException as e:
        return Err(HttpError(status=None, message=f"network failure: {e}"))
    if resp.status_code == 404:
        return Err(HttpError(status=404, message=f"no such repo: {owner}/{name}"))
    if resp.status_code >= 400:
        return Err(HttpError(status=resp.status_code, message=resp.text[:200]))
    try:
        return Ok(Repo.model_validate(resp.json()))
    except Exception as e:
        return Err(HttpError(status=resp.status_code, message=f"parse error: {e}"))


def fetch_with_retry[T, E](
    fetch: Callable[[], Result[T, E]], attempts: int = 3, backoff: float = 1.0
) -> Result[T, E]:
    last: Result[T, E] = fetch()
    i: int = 1
    while i < attempts:
        match last:
            case Ok(_):
                return last
            case Err(_):
                time.sleep(backoff * float(i))
                last = fetch()
                i = i + 1
    return last


def post_json(
    url: str, body: dict[str, object]
) -> Result[dict[str, object], HttpError]:
    try:
        resp = requests.post(url, json=body, timeout=10.0)
        resp.raise_for_status()
        return Ok(resp.json())
    except requests.HTTPError as e:
        return Err(HttpError(status=resp.status_code, message=str(e)))
    except requests.RequestException as e:
        return Err(HttpError(status=None, message=str(e)))


from typing import Callable


def main() -> None:
    result: Result[Repo, HttpError] = fetch_with_retry(
        lambda: fetch_repo("python", "cpython"), attempts=3, backoff=0.5
    )
    match result:
        case Ok(repo):
            print(f"{repo.full_name}")
            print(f"  stars: {repo.stargazers_count}")
            print(f"  lang:  {repo.language}")
            print(f"  desc:  {repo.description}")
        case Err(e):
            print(f"failed [{e.status}]: {e.message}")


if __name__ == "__main__":
    main()
