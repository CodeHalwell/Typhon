# tyc::kind_mismatch

Fires when a higher-kinded type constructor is applied **ill-kindedly** — either
to the wrong number of type arguments, or bound to two conflicting concrete
constructors in the same call. Typhon's HKT unification (`class Functor[F[_]]:`)
binds a constructor variable like `F` against a concrete head (`list`, `Box`,
…); when that binding can't be made consistently, the unifier reports this code
instead of silently degrading to `Unknown`.

## Examples

**Wrong arity** — `F` expects one type argument but is applied to two:

```ty
class Functor[F[_]]:
    def fmap[A, B](self, fa: F[A], f: Callable[[A], B]) -> F[B]: ...

# applying the constructor to the wrong number of arguments
# error: type constructor `F` applied to 2 argument(s) but expects 1
```

**Conflicting binding** — `F` is pinned to two different constructors in one
call:

```ty
# one argument is a `list[int]`, another forces `F = Box`
# error: type constructor `F` is bound to both `list` and `Box` in the same call
```

## Why

A constructor variable must resolve to exactly one type constructor of the right
arity for the substitution into the return type to be sound. Applying `F[_]` to
two arguments (`F[A, B]`), or unifying `F` against both `list` and `Box`, leaves
the kind unsatisfiable — so the checker reports it at the application site rather
than producing an unusable inferred type.

## Fix

- Apply the constructor to exactly the number of type arguments its kind
  declares (`F[_]` takes one).
- Pass arguments whose outer constructors agree, so the constructor variable
  resolves to a single type.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/kind_mismatch.md
