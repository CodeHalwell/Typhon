# tyc::lazy_usage

Fires when a `lazy` construct is used in an unsupported form. The supported
shapes are `lazy import name = module` and `lazy val NAME: T = expr` — any
other syntax under the `lazy` keyword is rejected.

## Example

```ty
lazy from heavy import Thing  # error: unsupported lazy form
```

## Why

`lazy` carries specific semantics: defer evaluation until first access. The
two supported forms each have a well-defined desugaring (a thread-safe
lookup for `lazy val`, an `__getattr__`-based stub for `lazy import`). Other
shapes don't have a clear desugaring and would either silently lose
laziness or produce surprising runtime behaviour.

## Fix

Use one of the supported forms:

```ty
lazy import thing = heavy.Thing
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/lazy_usage.md
