# typhon-graphql-server

An in-memory, statically typed GraphQL-style query engine written in Typhon.
The server parses nested query strings (e.g.
`users(role:admin) { id name posts { title author { name } } }`),
authenticates each request via a role-based token, executes the query through
generic `DataLoader`-batched resolvers, and returns a tree of resolved fields
modelled as a sealed union. No HTTP transport — this is a focused stress test
of the language, not a network library.

## Run

```bash
cd examples/apps/06-graphql-server
tyc check src/        # parse + resolve + type-check
tyc build             # emit clean Python to build/
python build/main.py  # run the demo queries
```

## Typhon features exercised

- `pub newtype` for `UserId`, `PostId`, `CommentId`, `Token` — distinct ID kinds at the type level.
- Sealed unions with deeply nested variants: `ResolvedField`, `AuthError`, `QueryNode`.
- Factory helpers per variant inside the defining module to bridge cross-module variant-to-union upcasting (Round 1 friction #1).
- Generic class + generic `impl[K, V]` for the `DataLoader[K, V]` cache.
- `Result[T, E]` everywhere on fallible paths; `?` propagation and `with ... else err` chains for parse + auth + resolve sequencing.
- Exhaustive `match` over `ResolvedField`, `AuthError`, and `QueryNode`, with the `raise RuntimeError("unreachable")` Round 1 workaround.
- Recursive-descent parser written entirely in straight-line Typhon (no `re`), producing a recursive AST that resolvers walk.
- Role-based middleware as a pure `Result`-returning function (no interfaces — Round 1 friction #2 workaround).
