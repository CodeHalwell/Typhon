# 05 — Concurrent web crawler

A polite, concurrent web crawler with content extraction, deduplication,
robots.txt compliance, and rate-limited per-host fetch. Designed to
stress async/await, `gather`, and `go` patterns.

- **Async worker pool** — N coroutines pulling URLs from a shared frontier
- **Per-host rate limiting** — token bucket per origin
- **robots.txt** parsing + caching
- **Content extraction** — title, text, outbound links
- **Deduplication** — content hash + URL canonicalisation
- **Retry with exponential backoff** + max-attempt cap + jitter
- **Sealed-union fetch outcomes**
- **Best-effort gather** when a page references multiple linked resources
- **Crawl report** projection

## Files

| File | Responsibility |
|---|---|
| `src/ids.ty` | `newtype` IDs (`Url`, `Host`, `DocumentId`) |
| `src/config.ty` | `freeze let` crawl config + `comptime let` user-agent |
| `src/frontier.ty` | Async-safe priority frontier + seen-set |
| `src/ratelimit.ty` | Token-bucket rate limiter per host |
| `src/robots.ty` | robots.txt fetcher + matcher with caching |
| `src/fetcher.ty` | HTTP fetch with retry/backoff |
| `src/extract.ty` | HTML → title/text/links |
| `src/dedup.ty` | Content-hash deduplication |
| `src/crawler.ty` | Worker loop tying it all together |
| `src/report.ty` | Final stats projection |
| `src/main.ty` | CLI entry point |

## Features exercised

- `async def` + `await` everywhere
- `gather(strategy="best-effort"):` for fanout fetches
- `go fetch(...)` for spawning workers via the runtime registry
- `lazy import` for heavy optional deps
- `newtype` for `Url`, `Host`, `DocumentId`
- `freeze let` config + `comptime let` user-agent
- Sealed-union fetch outcomes: `Fetched | NotModified | RobotsBlocked | RateLimited | TransientError | PermanentError`
- `Result[Document, FetchError]` from every fetcher
- `with`-chained `?` for fetch → extract → dedup → store
- `pub` markers throughout

## Running

```bash
cd examples/apps/05-web-crawler
tyc check src/
tyc build
python build/main.py https://example.com --max-pages 20
```
