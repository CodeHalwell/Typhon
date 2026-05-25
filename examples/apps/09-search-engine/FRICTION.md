# 09-search-engine — Round 2 friction log

Notes captured while writing the search-engine app under
`examples/apps/09-search-engine/`. The 12 friction items in
`examples/apps/TYPHON_FEEDBACK.md` were applied preemptively, so this
list focuses on **new** rough edges that surfaced from the recursive
query AST, dict-of-list inverted index, BM25 float math, phrase
matching over `tuple[int, ...]` positions, and the ranking pipeline.

Round 1 issues that still bit (no new info, just confirming they
re-appear in this app):

- #1 (variant→union upcast across modules) — every `Query` construction
  in callers had to route through `make_term` / `make_phrase` /
  `make_and` / `make_or` / `make_not` factories living in `query.ty`.
- #5 (exhaustive `match` doesn't satisfy `missing_return`) — added
  `raise RuntimeError("unreachable")` after the `match q:` in
  `search._eval_docs`.
- #6 (per-arm `let` shadow) — renamed `parts` → `and_parts` /
  `or_parts` and `terms` → `phrase_terms` across the sibling arms of
  `_eval_docs` and `_collect_terms`.
- #3 (`pub freeze let` parse-rejects) — `STOPWORDS` is `freeze let`
  without `pub`; the tokenizer keeps the constant private and exposes
  `is_stopword(...)` as the public API.

The new items below were not on the Round 1 list.

## 1. Recursive sealed unions force factory layering even for trivial wraps (severity: MEDIUM)

Code that broke / felt awkward:

```ty
# Inside the parser, every leaf and combinator wants to flow into
# the recursive `Query` union — but every `Ok(...)` carrying a variant
# would have failed cross-module upcasting if any consumer ever lived
# outside `query.ty`.
return Ok(QAnd(parts=parts))            # would break for cross-module callers
return Ok(QNot(inner=self.parse_atom()?))
```

Workaround applied:

```ty
# Same-module factories that just rewrap the constructor with the
# union return type. Five of them for one five-variant union.
pub def make_term(term: str) -> Query:   return QTerm(term=term)
pub def make_phrase(terms: list[str]) -> Query: return QPhrase(terms=terms)
pub def make_and(parts: list[Query]) -> Query:  return QAnd(parts=parts)
pub def make_or(parts: list[Query]) -> Query:   return QOr(parts=parts)
pub def make_not(inner: Query) -> Query: return QNot(inner=inner)

return Ok(make_and(parts))
return Ok(make_not(self.parse_atom()?))
```

Why this is a weakness: recursive ASTs are the textbook motivating
example for sealed unions, and they're exactly the case where the
variant→union upcast fails to elaborate. The factory layer is pure
boilerplate that grows linearly with the variant count.

## 2. `tuple(list[int])` for a `tuple[int, ...]` field needs an explicit annotation (severity: LOW)

Code that broke / felt awkward:

```ty
let posting: Posting = Posting(
    doc_id=doc.id,
    tf=len(positions),
    positions=tuple(positions),    # inferred as `tuple[int, ...]`?
)
```

Workaround applied:

```ty
let frozen_positions: tuple[int, ...] = tuple(positions)
let posting: Posting = Posting(
    doc_id=doc.id,
    tf=len(positions),
    positions=frozen_positions,
)
```

Why this is a weakness: hoisting a local just to annotate a single
expression is friction every time a frozen value class has a
`tuple[T, ...]` field built from a `list[T]`. A `tuple.__call__`
inference rule that picks up the surrounding field type would remove
the local entirely.

## 3. `dict[K, list[V]]` append-or-create has no idiomatic spelling (severity: MEDIUM)

Code that broke / felt awkward:

```ty
# Wanted: per_term_positions.setdefault(tok, []).append(pos)
# But `setdefault` returns `object` in the checker, so the .append
# call doesn't type-check.
per_term_positions.setdefault(tok, []).append(pos)   # tyc::missing_attribute
```

Workaround applied:

```ty
let existing: list[int]? = per_term_positions.get(tok)
if existing is None:
    per_term_positions[tok] = [pos]
else:
    let lst: list[int] = existing
    lst.append(pos)
    per_term_positions[tok] = lst    # write-back is technically
                                     # redundant (lst is the same
                                     # ref), but reads more honestly.
```

Why this is a weakness: inverted indexes, adjacency lists, and
group-bys all live on this pattern. Four lines per insert vs. one
line `setdefault(...).append(...)` is a real per-call-site tax in a
file that touches it every iteration of the indexing loop.

## 4. `dict.get(k) is True` is the cleanest set-membership probe (severity: LOW)

Code that broke / felt awkward:

```ty
# Wanted: `if k in seen:` where `seen: dict[int, bool]`.
# `in` on a dict works, but combined with later updates the checker
# narrows oddly and we couldn't rely on a uniform "set of ints" idiom.
mut seen: dict[int, bool] = {}
if k in seen:        # works, but reads as "k is a key" not "we have seen it"
    continue
seen[k] = True
```

Workaround applied:

```ty
mut seen: dict[int, bool] = {}
if seen.get(k) is True:
    continue
seen[k] = True
```

Why this is a weakness: Typhon doesn't appear to surface `set[T]` /
`frozenset[T]` as a first-class collection in the example apps, so
every "have we seen this DocId" question becomes a `dict[int, bool]`
+ `.get(k) is True` probe. A native `set[T]` would shrink each of
`_intersect`, `_union`, `_difference`, `_dedupe_strs`, and
`_matched_terms_for_doc` by a third.

## 5. `list[T].sort(key=...)` requires a top-level named callback (severity: LOW)

Code that broke / felt awkward:

```ty
hits.sort(key=lambda h: h.score, reverse=True)
# lambda forms with annotated parameters are not consistently
# accepted across the apps I've seen — and unannotated lambdas trip
# `no-implicit-any`.
```

Workaround applied:

```ty
def _hit_sort_key(h: ScoredHit) -> float:
    return h.score

hits.sort(key=_hit_sort_key, reverse=True)
```

Why this is a weakness: a top-level helper per sort key inflates the
file with names that exist only to satisfy a one-liner. A typed
lambda syntax (e.g. `(h: ScoredHit) -> float => h.score`) or
inference of the parameter type from `list[T].sort` would clean this
up everywhere a list-of-records gets sorted.

## 6. Iterating `tuple[int, ...]` to build a `list[int]` has no `list(t)` shortcut (severity: LOW)

Code that broke / felt awkward:

```ty
# Wanted:  return list(p.positions)
# `list(...)` on a generic-arity tuple wasn't obviously supported in
# the typing of the example apps, so I expanded by hand.
return list(p.positions)
```

Workaround applied:

```ty
mut out: list[int] = []
for pos in p.positions:
    out.append(pos)
return out
```

Why this is a weakness: a frozen `tuple[int, ...]` is a natural shape
for "fixed positional postings"; the asymmetry where `tuple(list[T])`
needs the workaround in (2) and `list(tuple[T, ...])` needs this
manual loop makes round-tripping between the two collection types
unnecessarily verbose.

## 7. BM25 float math is fine, but `float()` casts of `newtype X = int` are everywhere (severity: LOW)

Code that broke / felt awkward:

```ty
let n: float = idx.total_docs           # ❌ int → float not implicit
let df: float = idx.df(term)            # ❌ same
let want: int = doc_id                  # ❌ DocId → int not implicit
```

Workaround applied:

```ty
let n: float = float(idx.total_docs)
let df: float = float(idx.df(term))
let want: int = int(doc_id)             # newtype unwrap
```

Why this is a weakness: BM25 (and any numeric scorer) mixes `int`
sizes, `float` averages, and `newtype` IDs. The triple of `int(...)`
on a newtype, `float(...)` on the underlying int, and the surrounding
arithmetic ends up being most of the line count of a scoring file.
This is correct behaviour for `newtype` (the whole point is to force
a conversion), but a `from_int`/`to_int` operator that read as a noun
(`DocId.int`, `doc_id as int`) would carry the intent better than
`int(doc_id)`.

## 8. No "match arms must each end in expression" form for non-`None` returns (severity: LOW)

Code that broke / felt awkward:

```ty
# Wanted a `match` expression like:
let kind: str = match nx.kind:
    case "and": "and"
    case "or":  "or"
    case _:     "word"
```

Workaround applied:

```ty
# Repeated if/elif/else against string kinds.
if word == "AND":
    out.append(QToken(kind="and", value=word, pos=word_start))
elif word == "OR":
    out.append(QToken(kind="or", value=word, pos=word_start))
elif word == "NOT":
    out.append(QToken(kind="not", value=word, pos=word_start))
else:
    out.append(QToken(kind="word", value=word, pos=word_start))
```

Why this is a weakness: lexers in particular are mostly "classify a
string literal into a tag" — a match-as-expression would compress the
hot path of every tokenizer in the language. (This is not unique to
Typhon, but it's still friction worth listing because the alternative
forms — `dict[str, str]` lookup tables, function dispatch tables —
all individually trip on other items above.)
