# 2026-09-01 beta-readiness review — verified reproductions

Raw reproduction files from the 2026-09-01 beta-readiness review
(`docs/beta-readiness-review-2026-09-01.md`). Six parallel reviewers read
one area of the compiler each and had to *reproduce* every finding against
the release `tyc` binary and CPython 3.13 before reporting it; these are the
programs they used. Like every other `stress/round-*` directory this is an
internal corpus, not a tutorial: it deliberately contains programs that are
**expected to fail** (negative fixtures the checker should reject), programs
that expose a still-open bug, and programs that were fixed in the same
change and now pass.

| Directory | Reviewer scope | What the files exercise |
|---|---|---|
| `syntax/` | `tyc-syntax` (the text preprocessor) | guard one-liner line-count desync, `\|>` with comments / non-ASCII / lambdas, `?` on one-line compound statements, `while …?: … else`, `lazy let` in `class!`, `enum` / `gather:` / `impl` body collection, `as!` in `for`, `pub *` without a trailing newline, BOM, tabs |
| `types/` | `tyc-types` (the checker) | nullable operands, un-awaited coroutines, container-method arguments, slots attribute writes, lambda bodies, `T` inside generic bodies, builtin call arguments, `except` narrowing, `**kwargs` vs `Callable`, interface async/property/frozen conformance, `NoReturn`, `isinstance` tuples, bare `self` under `impl[T]`, field overrides, `int ** int`, unpack arity — plus the `fp*` false-positive probes and the `c*` shape probes that were checked for crashes (all clean; the handful nested deeper than CPython's own parser limit were dropped from the corpus, since CPython cannot run the emitted program either) |
| `emit/` | `tyc-desugar` / `tyc-emit` / `tyc-format` | `yield` precedence, one-element tuple patterns, `*` / `**` unpack operands, `as` inside or-patterns, non-finite float literals, NUL bytes, nested f-string format specs under `tyc fmt`, `ClassVar` cloning into subclasses, `class!` grandchildren, non-empty mutable defaults, `freeze let` over `enum` members, the `lazy let` proxy |
| `vm/` | `tyc-vm` | `sys.exit`, format specs, `next` / `enumerate` / `list.index`, generator laziness, `re`, `json`, `Counter` / `deque`, `sum` of floats, `%s` dispatch, `KeyError` payloads, `bool` bitwise ops, `datetime` / `pathlib` shims, Unicode string predicates |
| `analyse/` | `tyc-resolve` / `tyc-analyse` | `+=` on a `let`, `try` handlers as sibling arms, class-scope visibility, definite assignment gaps, f-string format-spec walking, prelude names, purity verdicts feeding `auto-memoise`, `auto-gather` name collisions, the accumulator-loop reduction, comptime folding |
| `tooling/` | `tyc` CLI | diagnostic line numbers after a `?` / `gather:` / with-chain expansion |

The `--filter round-2026-09-01` scope of `scripts/vm-differential.sh` runs
every file here both ways; units that diverge are pinned in
`scripts/differential-baseline.txt` and are bugs to burn down. Most are VM
gaps, but a few pin the opposite failure — the VM runs the program and the
*compiled* output does not (`exit cpython=1 vm=0` in the report): those are
desugar / emitter / checker holes, listed as open items in the review
document. Probes whose output is nondeterministic by construction (unseeded
`random`, string-hash ordering) were not kept, because the harness excludes
them from the verdict anyway. The review document lists which findings were
fixed in the same change and which remain open.
