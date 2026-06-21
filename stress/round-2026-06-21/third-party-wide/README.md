# Wide third-party introspection audit — 2026-06-21

An adversarial audit of Typhon's venv signature introspection (`tyc-venv`
→ `tyc-types`) across **43 popular third-party libraries** spanning data /
numeric, ML, web, HTTP, CLI, validation, DB, async, viz, cloud and utility
use cases — including libraries whose import name differs from the PyPI name
(`scikit-learn→sklearn`, `pillow→PIL`, `beautifulsoup4→bs4`, `pyyaml→yaml`,
`python-dateutil→dateutil`, `psycopg2-binary→psycopg2`,
`google-cloud-storage→google`).

See **[`findings.md`](findings.md)** for the full report (library matrix,
ranked issues, root causes, fixes, deferred items, executive summary).

## Layout

- `must_pass/` — 43 idiomatic, correct programs (one per library). Each
  **must** type-check clean; any error is a **false positive** (critical).
- `must_fail/` — 15 programs that omit a required argument, pass a
  wrong-typed argument, or use an unknown kwarg, for constructors, free
  functions **and** methods. Each **must** fail `tyc check` with
  `tyc::missing_argument` / `tyc::type_mismatch` / `tyc::unknown_kwarg`.
- `harness.sh` — runs `tyc check` over both buckets and classifies every
  result (PASS-OK / PASS-FALSEPOS / FAIL-CAUGHT / FAIL-MISSED).
- `requirements.txt` — the exact library versions audited.
- `proj/` — **generated** (project + `.venv` + build output). Gitignored;
  never committed.

## Requirements

Introspection reads the project's `.venv`, so the libraries must be
installed there (this is also how the dist→import name resolution works,
via `.dist-info` metadata):

```bash
cd tyc && cargo build --release --bin tyc && cd ..
cd stress/round-2026-06-21/third-party-wide
python3.13 -m venv proj/.venv
proj/.venv/bin/pip install -r requirements.txt
bash harness.sh        # TYC=/path/to/tyc overridable
```

## Result (tyc 0.15.6 + this round's fixes)

```
must_pass: ok=43 false_positive=0
must_fail: caught=15 missed=0
```

On the pre-fix `tyc 0.15.6` binary the same corpus missed **7/15**
must_fail cases (flask ×3, django/dateutil/rich/starlette nested ×4) while
still reporting 0 false positives. Two compiler fixes landed this round
recover all 7 with no new false positives:

1. `tyc-venv` — introspection no longer crashes the whole module when a
   member (e.g. a Flask/werkzeug `LocalProxy`, Django `LazySettings`) raises
   from `inspect.signature` / `callable()`.
2. `tyc-types` — multi-segment attribute calls (`pkg.sub.Thing()`) now flow
   through the same constructor/function/arity check as the `from`-import
   form.

The full workspace test suite (`cargo test --release`) and the
`stress/round-2026-06-21/repros/` corpus (130/130 check+build) remain green.
