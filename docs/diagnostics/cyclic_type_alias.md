# tyc::cyclic_type_alias

Fires when a `type` alias chain forms a cycle (`type A = B; type B = A`).
The runtime never crashes because Python evaluates aliases lazily, but no
caller can ever resolve the type, so every use of the alias propagates the
cycle.

## Example

```ty
type A = B
type B = A  # error: type alias `B` is part of a cycle
```

## Why

A cyclic alias has no fixed point — there's no concrete type the names ever
expand to. Any annotation using `A` or `B` would silently degrade to `Any`
under a lazy expansion, hiding type errors throughout the module.

## Fix

Break the cycle by anchoring one of the aliases to a concrete type, or
remove one of the declarations entirely:

```ty
type A = int
type B = A
```

See https://typhon.dev/lang/diagnostics/cyclic_type_alias
