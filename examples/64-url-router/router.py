from __future__ import annotations
from typhon_runtime import Ok, Err, Result
import dataclasses
from typing import Callable


@dataclasses.dataclass(slots=True)
class NoMatch:
    path: str


@dataclasses.dataclass(slots=True)
class Route:
    pattern: list[str]
    handler: Callable[[dict[str, str]], str]


@dataclasses.dataclass(slots=True)
class Router:
    routes: list[Route] = dataclasses.field(default_factory=list)

    def add(self, pattern: str, handler: Callable[[dict[str, str]], str]) -> None:
        self.routes.append(Route(pattern=split_path(pattern), handler=handler))

    def dispatch(self, path: str) -> Result[str, NoMatch]:
        parts: list[str] = split_path(path)
        for route in self.routes:
            params: dict[str, str] = {}
            if try_match(route.pattern, parts, params):
                return Ok(route.handler(params))
        return Err(NoMatch(path=path))


def split_path(path: str) -> list[str]:
    return [p for p in path.strip("/").split("/") if p != ""]


def try_match(pattern: list[str], parts: list[str], params: dict[str, str]) -> bool:
    if len(pattern) != len(parts):
        return False
    for i in range(len(pattern)):
        p: str = pattern[i]
        if p.startswith(":"):
            params[p[1:]] = parts[i]
        elif p != parts[i]:
            return False
    return True


def main() -> None:
    r: Router = Router()
    r.add("/", lambda _: "home")
    r.add("/users/:id", lambda params: f"user profile for #{params['id']}")
    r.add(
        "/users/:id/posts/:slug",
        lambda params: f"post '{params['slug']}' by #{params['id']}",
    )
    r.add("/about", lambda _: "about page")
    test_paths: list[str] = [
        "/",
        "/users/42",
        "/users/7/posts/intro-to-typhon",
        "/about",
        "/missing",
    ]
    for path in test_paths:
        match r.dispatch(path):
            case Ok(body):
                print(f"{path:<40} -> {body}")
            case Err(e):
                print(f"{path:<40} -> 404 (not found)")


if __name__ == "__main__":
    main()
