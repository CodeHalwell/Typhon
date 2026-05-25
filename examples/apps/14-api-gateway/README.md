# 14 — API gateway / service mesh

A production-shaped L7 reverse proxy / API gateway with:

- **Route table** matched by method + path glob (`/foo/*`, `/foo/**`, exact)
- **Upstream pool** per route, with pluggable load-balancing strategies
  (round-robin, least-connections, random, weighted)
- **Typed middleware chain** — `logging → tracing → routing → auth → rate-limit`
  composed via a `Middleware` concrete class with `Callable` pre/post hooks
- **Circuit breaker** per upstream — sealed-union `BreakerState`
  (`Closed → Open → HalfOpen`) with cooldown and probe gating
- **Token-bucket rate limiter** per (route, client) keyed on monotonic clock
- **Retry policy** — exponential backoff with jitter, max attempts, per-error
  predicates (`is_retryable_status`, `is_retryable_exception`)
- **Health checks** — periodic async probes against each upstream marked
  up/down; downed upstreams excluded from rotation
- **Auth middleware** — bearer-token verification against an in-memory store
- **Observability** — structured access log + Prometheus-style `/metrics`
  with reservoir-sampled p50/p99 latency per route
- **Config reload** — `POST /_gateway/reload` re-reads `routes.json` and
  swaps the route table + balancers atomically
- **HTTP server**: FastAPI (catch-all `proxy` route + `/_gateway/*` control
  plane); upstream calls go through `httpx.AsyncClient`

## Files

| File | Responsibility |
|---|---|
| `src/domain/ids.ty` | `newtype` wrappers (`RouteId`, `UpstreamId`, `ClientId`, `RequestId`, `TraceId`) |
| `src/domain/models.ty` | Internal `HttpRequest` / `HttpResponse`, sealed-union `GwError`, factories |
| `src/runtime/config.ty` | `freeze let` policy constants, `comptime let` build version, `load_config` |
| `src/balance/upstream.ty` | `Upstream` struct + healthy-pool filter |
| `src/balance/balancer.ty` | Sealed-union `BalanceStrategy` + concrete `Balancer` (Callable field) |
| `src/routing/routes.ty` | `Route`, `RouteTable`, sealed-union `MatchResult`, glob match |
| `src/policies/breaker.ty` | Circuit breaker (sealed-union `BreakerState` + per-upstream state) |
| `src/policies/limiter.ty` | Token-bucket rate limiter (per route × client) |
| `src/policies/retry.ty` | Sealed-union `RetryOutcome`, backoff, retryable-error predicates |
| `src/balance/health.ty` | Periodic async probes via `go` + `gather:` |
| `src/gateway/forward.ty` | Upstream forwarder (httpx) with breaker + retry integration |
| `src/gateway/middleware.ty` | `Middleware` chain (pre/post hooks), unified `handle_request` |
| `src/runtime/metrics.ty` | `MetricsRegistry`, reservoir sampling, Prometheus render |
| `src/policies/auth.ty` | In-memory token store |
| `src/domain/wire.ty` | Pydantic `model`s at the FastAPI boundary |
| `src/routing/loader.ty` | Read + validate `routes.json` → `RouteTable` |
| `src/gateway/server.ty` | FastAPI app, lifecycle, control-plane endpoints |
| `src/main.ty` | Entry point (`asyncio.run`, signal handling, uvicorn) |

## Features exercised

- `newtype RouteId = str`, `UpstreamId = str`, `ClientId = str`,
  `RequestId = str`, `TraceId = str`
- `freeze let DEFAULT_RETRY`, `DEFAULT_BREAKER`, `DEFAULT_RATE_LIMIT`,
  `DEFAULT_HEALTH` (deep-immutable policy constants)
- `comptime let BUILD_VERSION`, `DEFAULT_PORT`, `DEFAULT_ROUTES_PATH`
- Sealed unions with factories (Round-1 #1 workaround applied throughout):
  - `BreakerState = StateClosed | StateOpen | StateHalfOpen`
  - `BalanceStrategy = StratRoundRobin | StratLeastConn | StratRandom | StratWeighted`
  - `MatchResult = MatchHit | MatchMiss | MatchMethodMismatch`
  - `GwError = NoRouteError | NoUpstreamError | UpstreamFailure | BreakerOpen | RateLimitExceeded | AuthRequired`
  - `RetryOutcome = RetryOk | RetryAgain | RetryGiveUp`
- 5-layer middleware chain composed via `Callable` fields on a concrete
  `Middleware` class (Round-1 #2 workaround)
- `Result[T, E]` with three distinct error types: `GwError`, `LoadError`, `str`
- `gather:` for parallel health-check probes (warmup round)
- `go run_health_loop(registry) -> task` background prober
- Token-bucket using `time.monotonic()`
- Exhaustive `match` over `BreakerState`, `BalanceStrategy`, `MatchResult`,
  `GwError`, `RetryOutcome` — `raise RuntimeError("unreachable")` after each
- `pub` markers everywhere
- FastAPI + Pydantic `model` types at the HTTP boundary

## Running

```bash
cd examples/apps/14-api-gateway
tyc check src/
tyc build
python build/main.py
# in another shell:
curl http://localhost:8080/_gateway/health
curl http://localhost:8080/_gateway/metrics
curl -X POST http://localhost:8080/_gateway/reload
curl -H 'authorization: bearer dev-token' http://localhost:8080/api/users
```

Set `GW_ROUTES=/path/to/routes.json` to point at a real route file; without
one, a single fallback route is wired up that proxies `/**` to
`http://127.0.0.1:9001` + `:9002` (round-robin).
