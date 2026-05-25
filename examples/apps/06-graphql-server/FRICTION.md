# Friction encountered while building `06-graphql-server`

This app deliberately targets feature combinations that the five Round 1
apps did not exercise: a generic `DataLoader[K, V]` with `Callable` fields,
a hand-written tokenising parser that returns a `Result[..., str]` from
inside `impl` methods, a Pydantic-free domain layer that round-trips
nested objects through a deeply variant-rich sealed union, and a
role-based middleware that chains three independent `Result` values.

All twelve Round 1 issues fired at least once while writing this app —
nothing new there. The notes below are issues that *only* appear when
generics, `Callable` fields, and dict mutation are combined in the same
file. Severity reflects how often I had to detour around each one.

## 1. Bound-method values are unverified as `Callable[...]` field initialisers (severity: MEDIUM)

Code that felt awkward:

```ty
# in executor.ty
let user_loader: DataLoader[int, User] = new_loader(store.batch_load_users)
let post_loader: DataLoader[int, Post] = new_loader(store.batch_load_posts)
```

Where `DataLoader[K, V]` is declared with
`batch_fn: Callable[[list[K]], dict[K, V]]`. Round 1's `Handler` class
stored a `Callable` field but always populated it with a *top-level* `def`
(handlers.ty), never a bound method. The bound-method form is the natural
one for a `DataLoader` whose batch source is the `Store` instance, and
the emitted Python is fine — but `tyc` may not unify a bound-method type
with the declared `Callable[[list[int]], dict[int, User]]`.

Workaround applied: kept the bound-method form because the runtime is
fine, but left a comment in `executor.ty` warning future readers that if
the checker rejects this, the fix is to declare per-instance closures or
a tiny adapter class. The cleanest fallback (which I tried and reverted
because it added clutter without proven need) was:

```ty
def load_users(ids: list[int]) -> dict[int, User]:
    return store.batch_load_users(ids)
let user_loader: DataLoader[int, User] = new_loader(load_users)
```

— but Round 1 has no nested-`def` precedent either, so it's not obviously
safer.

Why this is a weakness: a generic data-loader is the canonical motivating
example for a `Callable` field, and the canonical *source* of the
callable is a bound method on a store/repo. If the checker can't see
that, every realistic generic-callable design has to route through a
top-level helper.

## 2. `dict.pop(k, default)` is awkward when `V` is a type parameter (severity: LOW)

Code that felt awkward:

```ty
impl[K, V] DataLoader[K, V]:
    def clear(self, key: K) -> None:
        let _discarded: V? = self.cache.pop(key, None)   # would need `Optional[V]` reasoning
        return None
```

The Python stdlib signature of `dict.pop(key, default)` returns
`Union[V, type(default)]`. With `default=None` and a Typhon-managed
generic `V`, naming the return type required `V?`, which then forces an
unused-variable workaround. `del self.cache[key]` would be cleaner but
isn't exercised by any Round 1 app, so I had no confidence the checker
accepts it.

Workaround applied: rebuilt the dict by iteration.

```ty
mut next_cache: dict[K, V] = {}
for (k, v) in self.cache.items():
    if k != key:
        next_cache[k] = v
self.cache = next_cache
```

Why this is a weakness: O(n) for a single-key delete is a real
performance smell in a hot DataLoader path; users will reach for
`del` or `pop` first. The language should document or verify support
for both inside generic `impl` blocks.

## 3. `Result`-returning helpers nested inside top-level dispatchers force re-annotation (severity: LOW)

Code that felt awkward:

```ty
def _parse_int_arg(args: dict[str, str], key: str) -> Result[int, str]:
    ...

pub def resolve_node(ctx: ResolverContext, node: QueryNode) -> Result[ResolvedField, str]:
    ...
    if name == "user":
        let uid_int: int = _parse_int_arg(args, "id")?
        return resolve_user(ctx, UserId(uid_int), sels)
    if name == "post":
        let pid_int: int = _parse_int_arg(args, "id")?
        ...
```

This is fine — but every call site has to spell out `let X: int = ...?`
even though the type is mechanically derivable from the helper's
`Result[int, str]` return. There is no Round 1 friction entry for this,
yet it adds ~20% line count to `resolvers.ty`.

Workaround applied: explicit `let X: int = ...?` at every call site.

Why this is a weakness: the `let`-annotation requirement is a hard
language rule (function locals must be annotated), which makes
`?`-chained code substantially noisier than the analogous Rust or Swift
construct where the binding is implicit.

## 4. Naming collision between `ids.Token` newtype and a natural `Token` AST class (severity: LOW)

Code that felt awkward:

```ty
# query.ty
pub class Token:                # the parser's lexeme type
    kind: str
    value: str

# executor.ty
from ids import Token           # the auth token newtype
from query import Token         # ❌ silent shadow
```

The names "Token" appear naturally in both an authentication layer
and a parser layer. Python's `from X import Y` allows the second import
to silently shadow the first; Typhon — being stricter elsewhere —
could plausibly catch this collision. Today it doesn't (or at least it
won't surface until the resolver phase, by which point the original
intent is lost).

Workaround applied: renamed the parser class to `Lexeme` throughout
`query.ty`.

Why this is a weakness: the friction is purely about naming, but in a
strict-superset language the user expects "unambiguous identifier"
guarantees comparable to Rust's `use` aliasing rules. A
`tyc::ambiguous_import` or `tyc::shadowed_import` diagnostic would have
caught it before any runtime hit.

## Confirmation of Round 1 issues hit here

While building, I deliberately pre-applied the Round 1 workarounds, so
the issues did not slow me down — but I did *encounter* the following:

- **#1 (variant→union upcast)**: every sealed union in this app
  (`ResolvedField` with 9 variants, `AuthError` with 3, `QueryNode` with
  2) required factory helpers in their defining module.
  `schema.ty` alone has 9 `make_field_*` factories.
- **#5 (exhaustive match still triggers `missing_return`)**: added
  `raise RuntimeError("unreachable")` after every exhaustive match in
  `schema.ty::kind`, `query.ty::node_*`, `auth.ty::auth_error_msg`,
  `executor.ty::execute`, and `main.ty::render`.
- **#7 (`dict.get(k) or default` doesn't narrow)**: used
  `x if x is not None else default` in `executor.ty::_required_role` and
  in `resolvers.ty::resolve_users` for the role argument.
- **#10 (unused-import flags type-only uses)**: `from schema import Comment`
  in `main.ty` and `from schema import Post, User` in `executor.ty` are
  used only inside generic-parameter positions (`DataLoader[int, User]`)
  — kept anyway, as the workaround note suggests.
