# 09-search-engine

A small in-memory full-text search engine, written in Typhon. It seeds
a tiny corpus of short documents, builds an inverted index with
positional postings, parses boolean queries (`AND`, `OR`, `NOT`,
parentheses, `"quoted phrases"`), evaluates them against the index,
ranks the surviving documents with BM25 and prints highlighted
snippets for the top hits. No external services; the whole pipeline
runs in one process and is chosen to stress Typhon's string-heavy
code, recursive sealed-union AST, dict-of-list inverted-index updates,
and BM25 float math.

## Run

```bash
cd examples/apps/09-search-engine
tyc check src/
tyc build
python build/main.py
```

## Typhon features exercised

- `pub newtype DocId = int` / `pub newtype TermId = int`
- `freeze let STOPWORDS = (...)` for the stopword tuple at module
  scope (dropped `pub` to work around the `pub freeze let` parse
  friction from Round 1)
- `pub class Posting frozen` for the value-shaped positional postings
- Mutable `class` for the growing `InvertedIndex` and recursive
  `QParser`
- `pub type Query = QTerm | QPhrase | QAnd | QOr | QNot` — a
  **recursive** sealed union where `QAnd.parts: list[Query]` and
  `QNot.inner: Query`, exercised by an exhaustive `match` in
  `_eval_docs` and `_collect_terms`
- Factory functions (`make_term`, `make_phrase`, `make_and`,
  `make_or`, `make_not`) in the union's defining module to bridge the
  cross-module variant-to-union upcast friction
- `Result[Query, QueryParseError]` plumbing through the parser with
  `?` propagation
- `impl` blocks for `InvertedIndex` and `QParser`
- Nullable accessors (`Document?`, `list[Posting]?`, `QToken?`) with
  `if x is not None` narrowing and per-arm rename of `let` bindings
  inside `match` arms
- `raise RuntimeError("unreachable")` after every exhaustive match
  that produces a value, to silence `tyc::missing_return`
- `tuple[int, ...]` for frozen position lists inside postings,
  iterated and converted via `tuple(list[int])`
- `math.log`, `float / int`, list `.sort(key=..., reverse=True)` for
  BM25 scoring and ranking
