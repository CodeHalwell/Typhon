# Confirmed VM ↔ CPython divergences

Programs here run correctly through `tyc build` + CPython and misbehave under
`tyc run`. They are checked in *because* they misbehave: the harness asserts
they **keep** differing, so fixing one fails the test — which is the prompt to
promote the file up a level into `examples/parity/`.

The six divergences the 2026-07-25 review found were all fixed and promoted.
The three below came from a second probe round and are still open:

| File | Divergence |
|---|---|
| `vm_regex_engine_limits.ty` | the VM's `re` is backed by the Rust `regex` crate, which refuses look-around and backreferences by design; CPython's backtracking engine accepts both |
| `vm_model_construction.ty` | a `model` field with a default is rejected by the VM's constructor, and `str`/`repr` leak the compiler-synthesised `model_config` |
| `vm_bytearray_missing.ty` | `bytearray` is absent from the VM's builtin table |

The regex one is architectural rather than an oversight — closing it means
adopting a backtracking engine and giving up the linear-time guarantee, which
is a dependency-policy decision, not a bug fix.

## When to put a file here

`tyc/crates/tyc/tests/parity_corpus.rs` asserts that every `.ty` in
`examples/parity/` produces identical stdout under `tyc run` and
`tyc build` + CPython. When you add a case and the two disagree, you have
found a bug. Fix it if you can. If you cannot fix it immediately, move the
program here rather than deleting it or weakening the assertion, and open it
with a `# DIVERGENT:` header recording:

- both outputs, labelled `CPython:` and `VM:`
- which one is correct, and the CPython rule that makes it so
- anything adjacent that *does* agree, so the next reader knows the blast radius

Files here are asserted to **keep** differing. That inversion is deliberate:
when someone fixes the underlying bug the test fails, which is the prompt to
`git mv` the file up a level, replace the `# DIVERGENT:` header with a short
description of what it exercises, and add a row to
[`../README.md`](../README.md).
