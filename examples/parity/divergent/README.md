# Confirmed VM ↔ CPython divergences

**This directory is currently empty, which is the goal state.** Every
divergence found so far has been fixed and its program promoted up a level
into `examples/parity/`, where it is now asserted to produce byte-identical
output under both execution paths.

It is kept so the next confirmed divergence has an obvious home.

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
