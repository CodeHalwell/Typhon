# Stress Round 2026-05-22

Fresh round of `.ty` stress tests covering language edge cases, IO,
ML/numpy, AI/LLM, agents, APIs, SDK patterns, meta-stress, and
diagnostic quality. Built against `tyc 0.3.0` (from-source on this
branch).

The write-up of new findings is in [`FINDINGS.md`](./FINDINGS.md) — 13
issues filed including 3 CRITICAL (silent-wrong-output and VM
mis-scoping). The biggest wins from this round:

- **N5 / N6** — silent paren-stripping in emit for `not (X or Y)` and
  `not (X if C else Y)`. Real-world hit in the ReAct-agent test.
- **N9** — VM `match` arm writes don't propagate to outer scope.
  Breaks every accumulator pattern; `tyc run` is currently the
  documented default.
- **N1 / N2 / N10 / N11 / N13** — five HIGH-severity holes around
  `freeze let`, `?`-in-comp, VM static checking, `tyc migrate`
  completeness, and `match self.<field>:` flow analysis.

## Layout

```
01-language-edge/    edge cases in core syntax + semantics
02-io/               file IO, pathlib, sqlite, async file
03-ml-numpy/         numpy ops, k-NN, linear regression, attention
04-ai-llm/           message threads, structured output, streaming, RAG, prompt templating
05-agents/           ReAct, state machines, multi-agent, tool dispatch
06-api/              mini HTTP router, validation pipeline, graphql-style resolver
07-sdk/              paginator, event bus, circuit breaker
08-meta-stress/      paren precedence, fstring edges, decorator order, large program, VM probes
09-error-quality/    deliberately-broken inputs that should produce specific tyc:: codes
```

## Run

```bash
# Build the compiler once.
cd ../../tyc && cargo build --release

# One-off — set up a venv for any test that needs pydantic / numpy / etc.
python3.13 -m venv /tmp/typhonvenv
/tmp/typhonvenv/bin/pip install pydantic numpy

# Run a single test.
PYTHON=/tmp/typhonvenv/bin/python RUN=1 ./run_one.sh 01-language-edge/01-deep-newtype-nesting.ty

# Show emitted Python.
SHOW_EMIT=1 RUN=1 PYTHON=/tmp/typhonvenv/bin/python ./run_one.sh <ty>

# Build only (no exec).
./run_one.sh <ty>

# Run the whole suite.
PYTHON=/tmp/typhonvenv/bin/python RUN=1 ./run_all.sh
```

Sidecar `<name>.deps` files would add `[dependencies]` entries (one
package per line) — none of the cases in this round needed them
because the venv carries everything.

`run_all.sh` reports pass / fail counts; the failing-by-design cases
(`09-error-quality/*`, the deliberate-mismatch probes) and the
real-bug repros (`N1`, `N2`, `N13`) are both surfaced as `FAIL`. See
`FINDINGS.md` for the per-bug repro mapping.
