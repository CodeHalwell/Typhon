from __future__ import annotations
from typhon_runtime import Ok, Err, Result
import dataclasses
import asyncio
import httpx
from bs4 import BeautifulSoup


@dataclasses.dataclass(slots=True)
class Article:
    title: str
    url: str
    excerpt: str | None


@dataclasses.dataclass(slots=True)
class ScrapeError:
    url: str
    reason: str


async def fetch(client: httpx.AsyncClient, url: str) -> Result[str, ScrapeError]:
    try:
        resp: httpx.Response = await client.get(
            url,
            headers={"User-Agent": "TyphonExampleBot/1.0"},
            follow_redirects=True,
            timeout=10.0,
        )
    except httpx.HTTPError as e:
        return Err(ScrapeError(url=url, reason=str(e)))
    if resp.status_code >= 400:
        return Err(ScrapeError(url=url, reason=f"status {resp.status_code}"))
    return Ok(resp.text)


def parse_articles(html: str, base_url: str) -> list[Article]:
    soup: BeautifulSoup = BeautifulSoup(html, "html.parser")
    items: list[Article] = []
    for h in soup.select("article h2 a, h2 a"):
        title: str = h.get_text(strip=True)
        href: str = h.get("href", "")
        if len(title) == 0 or len(href) == 0:
            continue
        full: str = (
            href
            if href.startswith("http")
            else base_url.rstrip("/") + "/" + href.lstrip("/")
        )
        excerpt_tag = h.find_next("p")
        excerpt: str | None = (
            excerpt_tag.get_text(strip=True) if excerpt_tag is not None else None
        )
        items.append(Article(title=title, url=full, excerpt=excerpt))
    return items


async def scrape(urls: list[str]) -> list[Result[list[Article], ScrapeError]]:
    limits: httpx.Limits = httpx.Limits(max_connections=4)
    results: list[Result[list[Article], ScrapeError]] = []
    async with httpx.AsyncClient(limits=limits) as client:

        async def one(u: str) -> Result[list[Article], ScrapeError]:
            fetched: Result[str, ScrapeError] = await fetch(client, u)
            match fetched:
                case Ok(html):
                    await asyncio.sleep(0.5)
                    return Ok(parse_articles(html, u))
                case Err(e):
                    return Err(e)
            raise RuntimeError("unreachable")

        results = await asyncio.gather(*[one(u) for u in urls])
    return results


async def main_async() -> None:
    targets: list[str] = ["https://news.ycombinator.com/", "https://lobste.rs/"]
    results: list[Result[list[Article], ScrapeError]] = await scrape(targets)
    for url, r in zip(targets, results):
        print(f"\n# {url}")
        match r:
            case Ok(articles):
                for a in articles[:5]:
                    print(f"  - {a.title}")
                    print(f"    {a.url}")
            case Err(e):
                print(f"  failed: {e.reason}")


def main() -> None:
    asyncio.run(main_async())


if __name__ == "__main__":
    main()
