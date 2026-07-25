# Typhon error examples — programs that are *meant* to fail

The rest of [`examples/`](../) shows Typhon working. This directory shows it
saying **no**, which is where most of the language actually lives: every rule
in the cheat sheet exists because some class of bug was worth refusing, and the
fastest way to learn a rule is to read the diagnostic that enforces it.

Each `.ty` file here is a small, deliberately broken program with a header
comment that names the diagnostics it produces, explains *why* the compiler
objects, and shows the fix. Run any of them:

```bash
tyc check examples/errors/01-bindings-and-mutability/immutable_assign.ty
tyc explain immutable_assign        # the full catalog entry, offline
```

Several files carry more than one error on purpose — see
[`11-multi-error-programs/`](11-multi-error-programs/), which is what a real
file looks like halfway through a migration.

## These files are enforced, not decorative

`tyc/crates/tyc/tests/error_examples.rs` walks this directory on every
`cargo test --workspace` and asserts that each file produces **exactly** the
diagnostics its header declares — no more, no fewer. A diagnostic that stops
firing, changes severity, or starts firing somewhere new breaks the build.

The header format is three directives:

```python
# EXPECT-ERROR: tyc::missing_binding_kind    # one per error code
# EXPECT-WARN:  tyc::unused_import           # one per warning/advice code
# REQUIRES: build                            # rare: needs `tyc build`, not `tyc check`
```

`REQUIRES: build` exists for diagnostics that can only fire during emission.
No file currently needs it — `tyc::contains_secret_literal` was the last
holdout, and as of v1.0.0-alpha.7 it fires under plain `tyc check` too, so
the documented CI gate (`tyc check src/`) sees every code in this corpus.

Codes are matched as a *set*, not a count: a file expecting
`tyc::implicit_any` passes whether the code fires once or three times.

> **`tyc fmt` note.** Four files here demonstrate forms that don't *parse*
> (`pub lazy let`, `model X frozen:`, a function-level `F[_]`, an unbracketed
> multi-line `|>`), so the formatter cannot process them — a repo-wide
> `tyc fmt examples/` will stop on the first one. Format this directory with
> `tyc fmt` per-file, or exclude it. Every other file here is fmt-clean.

## The lineup

| Directory | What it covers |
|---|---|
| [`01-bindings-and-mutability/`](01-bindings-and-mutability/) | Rule 2 — `let` vs `mut`, shadowing, declare-then-assign, parameter rebinding |
| [`02-types-and-annotations/`](02-types-and-annotations/) | Rules 1 and 3 — missing annotations, `T?` narrowing, mismatches, bare containers, tuple arity |
| [`03-classes-and-fields/`](03-classes-and-fields/) | Rule 4 — `impl` vs class body, `__init__`, `frozen`, field ordering, duplicate methods |
| [`04-sealed-unions-and-match/`](04-sealed-unions-and-match/) | Rule 6 — exhaustiveness, pattern arity, nullary variants, capture shadowing |
| [`05-result-and-errors/`](05-result-and-errors/) | Rule 7 — `?` placement, error-type mismatches, `raise`, missing returns |
| [`06-async-and-concurrency/`](06-async-and-concurrency/) | `await`, blocking calls in `async def`, missed `gather:` opportunities |
| [`07-interfaces-and-generics/`](07-interfaces-and-generics/) | Structural conformance, `isinstance` on interfaces, PEP 695, HKT limits |
| [`08-newtype-freeze-pub/`](08-newtype-freeze-pub/) | Nominal aliases, `freeze let` freezability, `pub` placement |
| [`09-boundaries-and-escapes/`](09-boundaries-and-escapes/) | `unsafe:` leaks, `as!`, `lazy import`, `extend`, forms that don't parse |
| [`10-footguns-and-lints/`](10-footguns-and-lints/) | Python's classic traps — mutable defaults, `is` on literals, loop closures, unmanaged resources, inlined secrets |
| [`11-multi-error-programs/`](11-multi-error-programs/) | Realistic files with many errors at once, including which errors *mask* others |
| [`12-known-gaps/`](12-known-gaps/) | The inverse: programs that compile clean and fail at runtime. See that directory's README |

## Start here

If you are new to Typhon, three files carry most of the signal:

1. [`11-multi-error-programs/python_habits.ty`](11-multi-error-programs/python_habits.ty)
   — ordinary Python, and every habit it asks you to change.
2. [`11-multi-error-programs/error_masking.ty`](11-multi-error-programs/error_masking.ty)
   — why a single misplaced `?` can hide every other error in a file.
3. [`12-known-gaps/missing_await_in_async_caller.ty`](12-known-gaps/missing_await_in_async_caller.ty)
   — the checker is strict, not omniscient; here is one place it is silent.

`12-known-gaps/` is the honest directory: code the compiler wrongly *accepts*
and that fails at runtime anyway. Its entries are asserted to keep
misbehaving, so fixing one fails the test — which is the prompt to delete or
reclassify the file. (A `13-false-positives/` directory recording the mirror
case — valid programs the checker wrongly *rejected* — lived here until its
last two entries, a `try`/`except`/`else` reachability hole and `case {}:`
being treated as refutable, were fixed; both shapes are now regression tests
in `tyc-types`.)

## Adding a new one

1. Write the smallest program that triggers the diagnostic, with a header
   comment explaining the rule and the fix — the prose is the point, not the
   broken code.
2. Run `tyc check` on it and copy the observed codes into `# EXPECT-` lines.
3. Run `cargo test --workspace error_examples` from `tyc/`.
4. If the diagnostic only appears under `tyc build`, add `# REQUIRES: build`.
