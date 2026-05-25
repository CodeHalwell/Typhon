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
| `src/domain/ids.ty` | `newtype` IDs (`Url`, `Host`, `DocumentId`) |
| `src/runtime/config.ty` | `freeze let` crawl config + `comptime let` user-agent |
| `src/crawl/frontier.ty` | Async-safe priority frontier + seen-set |
| `src/net/ratelimit.ty` | Token-bucket rate limiter per host |
| `src/net/robots.ty` | robots.txt fetcher + matcher with caching |
| `src/net/fetcher.ty` | HTTP fetch with retry/backoff |
| `src/content/extract.ty` | HTML → title/text/links |
| `src/crawl/dedup.ty` | Content-hash deduplication |
| `src/crawl/crawler.ty` | Worker loop tying it all together |
| `src/content/report.ty` | Final stats projection |
| `src/main.ty` | CLI entry point |

## Features exercised

- `async def` + `await` everywhere; workers spawned with `go worker_loop(...) -> task`
- `newtype` for `Url`, `Host`, `DocumentId`, `ContentHash`
- `freeze let` config + `comptime let` user-agent
- Sealed-union fetch outcomes — `Fetched | NotModified | RobotsBlocked | RateLimited | TransientError | PermanentError` — fed through an explicit `match` in `_process_item`
- `Result[Document, ExtractError]` returned by the HTML extractor; the
  rest of the pipeline switches on the sealed union directly
- Per-host token-bucket rate limiter, in-flight-counter termination
  (no fixed-duration sleep races), robots.txt caching with per-host async lock
- `pub` markers throughout

## Running

```bash
cd examples/apps/05-web-crawler
tyc check src/
tyc build
python build/main.py https://example.com --max-pages 20
```
