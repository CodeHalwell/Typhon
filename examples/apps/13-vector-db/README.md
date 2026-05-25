# 13 — In-memory vector database (HNSW + filter DSL)

A production-shaped in-memory vector database with:

- **Vector storage** — fixed-dimension `float` vectors packed in `list[float]`
- **HNSW index** — layered graph with M, ef-construction, ef-search params; insert and search
- **Multiple metrics** — cosine, euclidean (L2), inner product via a sealed `Metric` union
- **Filter DSL** — small AST for filtering on attached metadata: `Eq`, `Ne`, `Gt`, `Lt`, `In`, `And`, `Or`, `Not`
- **Filter parser** — turns `tags = "ai" AND year >= 2024` into the AST
- **Collections API** — create, insert/upsert, delete, search, get
- **Snapshotting** — binary snapshot + JSON metadata sidecar
- **HTTP server** (FastAPI) exposing the API
- **Concurrent access** — single-writer / many-reader pattern via `asyncio.Lock`
- **Lazy numpy import** — only loaded when batch-search is invoked

## Files

| File | Responsibility |
|---|---|
| `src/domain/ids.ty` | `newtype` wrappers (`VectorId`, `CollectionId`, `Dim`, `SnapshotSeq`) |
| `src/domain/dbmodels.ty` | HTTP `model` types + internal `class` types + sealed `Metric` union |
| `src/runtime/config.ty` | `freeze let` HNSW defaults + `comptime let` build constants |
| `src/vector/vector_ops.ty` | Pure list-of-float vector operations (cosine, l2, dot, norm) |
| `src/vector/metric.ty` | Metric dispatch (`score(metric, a, b)`) over the sealed union |
| `src/indexes/hnsw.ty` | Layered HNSW graph: insert + search |
| `src/filters/filter_ast.ty` | Sealed `FilterExpr` AST + factories + evaluator |
| `src/filters/filter_parse.ty` | Tokenizer + parser for the filter mini-language |
| `src/indexes/collection.ty` | Generic `Collection[D]` — insert/upsert/delete/get/search |
| `src/indexes/snapshot.ty` | Write / load snapshot (binary vectors + JSON metadata) |
| `src/transport/api.ty` | FastAPI control plane |
| `src/main.ty` | Wires everything together |

## Features exercised

- `newtype VectorId = int`, `newtype CollectionId = int`, `newtype Dim = int`, `newtype SnapshotSeq = int`
- `freeze let` for HNSW defaults (M, ef_construction, ef_search, max_level)
- `comptime let BUILD_TAG`, `comptime let DEFAULT_PORT`
- Sealed-union `Metric` (`MCosine`, `MEuclidean`, `MInnerProduct`) with factories
- Sealed-union `FilterExpr` (8 variants) with factories
- Generic `class Collection[D]`
- `lazy import np = numpy`
- `Result[T, E]` with `IndexError`, `ParseError`, `SnapshotError` error types
- `with`-chain `?` for the snapshot pipeline
- Exhaustive `match` over `Metric` and `FilterExpr`
- `pub` markers everywhere
- FastAPI with `model` request/response types

## Running

```bash
cd examples/apps/13-vector-db
tyc check src/
tyc build
python build/main.py
# in another shell:
curl -X POST localhost:8001/collections \
     -H 'content-type: application/json' \
     -d '{"name":"docs","dim":4,"metric":"cosine"}'
curl -X POST localhost:8001/collections/docs/vectors \
     -H 'content-type: application/json' \
     -d '{"id":1,"vector":[0.1,0.2,0.3,0.4],"metadata":{"year":2024,"tag":"ai"}}'
curl -X POST localhost:8001/collections/docs/search \
     -H 'content-type: application/json' \
     -d '{"vector":[0.1,0.2,0.3,0.4],"k":5,"filter":"year >= 2024"}'
```
