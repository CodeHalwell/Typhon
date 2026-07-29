# Differential and knob-coverage gates

Two CI gates, added in response to items **T0.2** and **T0.4** of
[`codebase-review-2026-07-28.md`](codebase-review-2026-07-28.md). Both live in
`scripts/`, run locally with no network access, and refuse to run at all rather
than run partially — a gate that passes vacuously is worse than no gate, because
it reads as coverage.

| Gate | Script | CI job | What it proves |
|---|---|---|---|
| VM ↔ CPython differential | `scripts/vm-differential.sh` | `differential` | `tyc run` behaves identically to `tyc build` + CPython 3.13 over the whole `.ty` corpus |
| Opt-in knob codegen matrix | `scripts/knob-matrix.sh` | `knob-matrix` | Every opt-in codegen knob actually fires, and does not change observable behaviour |

Both require a release binary and a CPython **3.13+** interpreter reachable as
`python3.13`:

```bash
cd tyc && cargo build --release && cd ..
```

---

## 1. VM ↔ CPython differential (T0.2)

### Why

`docs/vm.md` and `CLAUDE.md` both state the contract plainly: the in-process
tree-walking VM is a **drop-in** for `tyc build && python`, and a VM/CPython
divergence is a bug. Until this harness there was no automated differential
testing against CPython anywhere in CI, so the VM was a second, independently
written implementation of Python semantics whose agreement with the first was
entirely hopeful. The 2026-07-28 review's Cluster G attributes 9–14 findings to
exactly that gap.

### What it does

For every **unit** in `examples/` and `stress/`:

1. `tyc build` the unit and run the emitted `build/main.py` under `python3.13`;
2. run the same source through the VM with `tyc run`;
3. compare **stdout** and **exit code**.

A *unit* is one independently-executable thing:

* a **project** — any directory containing `typhon.toml` (unit id ends in `/`);
* a **standalone `.ty` file** — any `.ty` not inside a project directory. The
  harness synthesises a minimal project around it (`src/main.ty` plus a
  `format = false` `typhon.toml`), the same shape `stress/build_run.sh` has
  always used.

Nothing is executed in the working tree: every unit is copied into a scratch
directory first, and both sides run with the same fixed environment
(`PYTHONHASHSEED=0`, `TZ=UTC`, `LC_ALL=C.UTF-8`, stdin closed) and the same
cwd. `TYC_NO_SYNC` / `TYC_NO_INTROSPECT` keep the run network-free and stop
`tyc` from importing the host's site-packages.

### Result classes

| Class | Meaning | Gates? |
|---|---|---|
| `ok` | stdout and exit code agree | — |
| `diverge` | the VM disagrees with CPython | **yes** |
| `vacuous` | both sides failed with empty stdout — almost always an uninstalled third-party import. Reported separately and loudly, because counting these as passes would overstate coverage | no |
| `nobuild` | `tyc build` failed, so there is nothing to compare (chiefly the deliberately-invalid `stress/` repros and `must_fail/` fixtures) | no |
| `noentry` | built but emitted no `build/main.py` | no |
| `nondeterministic` | a side disagreed with **itself** across repeated runs (clocks, tempfile paths, task scheduling), so it cannot be used to judge the VM | no |
| `both-timeout` | both sides exceeded the per-side limit | no |

Nondeterminism is detected only on the divergent path, and by re-running *both*
sides twice more — so a clock-dependent program can never flake the gate red.

### The expectations file

`scripts/differential-baseline.txt` lists the unit ids that are known to
diverge, one per line, `#` for comments. The gate fails when:

* a unit diverges and is **not** listed — a regression; **and**
* a listed unit **stops** diverging — a stale entry.

Failing in both directions is deliberate: the baseline can only shrink, never
rot. **Every line in it is a VM bug.** It is a burn-down list, not an
allow-list.

A `--scope` / `--filter` run only compares against the slice of the baseline it
actually covered, so a partial run can flag a regression but can never report
the uncovered remainder as "fixed".

### Running it

```bash
scripts/vm-differential.sh                       # whole corpus, gate against the baseline
scripts/vm-differential.sh --scope examples      # examples/ only
scripts/vm-differential.sh --jobs 16             # parallelism (default: nproc)
scripts/vm-differential.sh --report r.tsv        # full per-unit TSV
scripts/vm-differential.sh --update              # rewrite the baseline from this run
```

Triaging one entry:

```bash
TMPDIR=/tmp/triage scripts/vm-differential.sh \
    --filter 'examples/57-iterators-generators' --keep
# then diff cpy.out / vm.out and read vm.err in the kept workdir
```

Runtime is roughly **75 s** for the full 1130-unit corpus at `--jobs 8` on a
4-core machine; CI runs it at `--jobs 4`.

### What the first run found

The 2026-07-28 review estimated "~37 known divergences". The measured figure on
the v1.0.0-alpha.6 tree is **126**, over 1130 units. They fall into four groups:

| Group | Count | Shape |
|---|---|---|
| VM runtime error | 58 | The VM raises where CPython succeeds — missing attributes on shim modules (`datetime.timezone`, `contextlib.suppress`, `sys.modules`, `typing.override`), enum member access on a class object, `functools.partial` / `itertools.product` refusing keyword arguments, missing builtins (`eval`, `bytearray`), `'instance' object is not subscriptable` |
| VM missing module | 32 | `ImportError: tyc-vm cannot import …` for stdlib modules with no shim — `tempfile`, `io`, `csv`, `sqlite3`, `argparse`, `threading`, `decimal`, `struct`, `string`, `operator`, `bisect`, `fractions`, `urllib`, `subprocess`, `__future__` |
| **Silently wrong output** | **27** | Both sides exit 0; the VM prints something different. The dangerous class |
| VM unsupported | 9 | An explicit `NotImplementedError` / `RuntimeError` — chiefly `@contextmanager` generators used as context managers, and the eager-generator materialisation cap |

The silent-wrong group is the one to fix first, because nothing else in the
toolchain will ever notice it. Representative cases:

* `raise X from Y` loses `__cause__` under the VM (`cause: None`);
* `model_dump_json()` emits pydantic-style compact separators under CPython and
  spaced `json.dumps` separators under the VM;
* a `model` instance's `model_dump()` / repr leaks a `model_config={}` field;
* `@cached_property` re-computes on every access;
* `freeze let` reprs as `mappingproxy({...})` instead of `{...}`;
* `functools.wraps` does not copy `__name__` (a decorated function reports
  `wrapper`);
* `re.findall` with groups returns whole matches instead of tuples;
* `str.format` width/alignment specifiers are ignored;
* module-level `lazy let` evaluates eagerly, reordering its side effects;
* a `TypedDict`-shaped value reprs as the class rather than a dict.

### Burning the baseline down

Fix the VM behaviour, delete the line, re-run. If a divergence genuinely cannot
be fixed in the change at hand, keep the line and add a `#` comment above it
saying why — but note that "the VM does not support X" is a bug report, not a
justification: the contract is a drop-in.

---

## 2. Opt-in knob codegen matrix (T0.4)

### Why

`examples/` and `stress/` run entirely on default configuration. Every opt-in
codegen path therefore shipped with **zero** end-to-end coverage: the
auto-parallel comprehension rewrite, the parallel reduction fold, both parallel
backends, PGO memoisation, `traceback-remap`, the free-threaded targets, the
PEP 810 lazy-import lowering. Cluster I of the review is specifically about
those rewrites changing program semantics once the knob is on.

### Fixture layout

Each `tests/knobs/<name>/` is a complete miniature project:

| File | Purpose |
|---|---|
| `typhon.toml` | the project **with the knob on** |
| `control.toml` | the same project **with the knob off** (optional) |
| `src/*.ty` | the source, shared by both builds |
| `emit-contains.txt` | substrings that must appear in the knob-on emitted Python |
| `emit-absent.txt` | substrings that must **not** appear in the knob-on build |
| `control-contains.txt` | substrings that must appear in the control build |
| `expect.txt` | exact expected CPython stdout |
| `stderr-contains.txt` | substrings required in CPython stderr |
| `typhon-profile.json` | committed profile data (`pgo-memoise` only) |
| `meta.conf` | `run=both\|none`, `expect-exit=N`, `vm-diverges=yes`, `requires-module=NAME` |

In any marker file a literal `\n` expands to a real newline, so one marker can
span source lines (e.g. a decorator plus the `def` it sits on).

### What each fixture asserts

1. the knob-on build **succeeds**;
2. `emit-contains` markers are present and `emit-absent` markers are absent —
   the knob **fired**;
3. with `control.toml`, every `emit-contains` marker is **gone** and every
   `control-contains` marker is **back** — which is what makes (2)
   knob-sensitive rather than trivially true;
4. the knob-on build runs under `python3.13` with exactly `expect.txt` on
   stdout and the expected exit code;
5. the knob-**off** build produces byte-identical stdout — the rewrite is
   semantics-preserving, which is the entire promise of an "opt-in
   optimisation";
6. `tyc run` agrees with CPython — unless the fixture declares
   `vm-diverges=yes`, in which case **agreement** is the failure, so that
   allowance cannot rot either.

### Current fixtures

| Fixture | Knob | Marker |
|---|---|---|
| `auto-memoise` | `[strictness] auto-memoise` | `@functools.cache` |
| `auto-gather` | `[strictness] auto-gather` | `asyncio.TaskGroup()` + `create_task` |
| `auto-parallel` | `[strictness] auto-parallel` | `typhon_runtime.parallel.map_pure` (bare map, `if` filter, and nested pure call with a loop-invariant arg) |
| `auto-parallel-reductions` | `[strictness] auto-parallel-reductions` | `sum(typhon_runtime.parallel.map_pure` |
| `parallel-backend-interpreters` | `[strictness] parallel-backend` | `_BACKEND = "interpreters"` in the generated `parallel.py` |
| `pgo-memoise` | `[strictness] pgo-memoise` + `pgo-min-calls` | `@functools.cache` on the hot fn and **not** on the below-threshold one |
| `optimise-level-1` | `[optimise] level = 1` | memoise **and** gather rewrites with no `[strictness]` entry naming them |
| `traceback-remap` | `[emit] traceback-remap` | `typhon_runtime.traceback` install + a `.ty`-attributed stderr traceback |
| `free-threaded-parallel` | `[python] target = "3.13t"` + `free-threaded` | the emitted Python still runs on a stock GIL 3.13 |
| `model-extra-allow` | `[emit] model-extra` | `ConfigDict(extra="allow")` |
| `skip-decoration-bases` | `[emit] skip-decoration-bases` | `@dataclasses.dataclass` suppressed on the listed base's subclass |
| `lazy-import-pep810` | `[python] target = "3.15"` | native `lazy import json as js` instead of the runtime proxy (build-only — python3.13 cannot parse PEP 810) |

### Running it

```bash
scripts/knob-matrix.sh
scripts/knob-matrix.sh --filter auto-parallel
scripts/knob-matrix.sh --filter pgo --keep     # keep both builds for inspection
```

Runtime is a few seconds. Adding a knob means adding a directory — no script
change.

### Missing runtime dependencies

A fixture may declare `requires-module=NAME` when its *execution* half needs a
third-party package (its codegen half never does). If the module is missing the
fixture drops to **build-only** and says so on its own result line *and* again
in the summary as `REDUCED to build-only`. It is never skipped silently: a skip
nothing observes is indistinguishable from a pass. CI installs `pydantic` so
the matrix runs at full coverage there; locally the script itself stays
network-free.

---

## 3. Known gaps

These are stated plainly rather than papered over.

* **Third-party-dependent corpus files cannot be differentially tested here.**
  Around 90 units import `numpy`, `torch`, `anthropic`, `fastapi`, `pytest` and
  friends. With no network, both execution paths fail on the import, so they
  land in the `vacuous` bucket. They are counted and reported separately, never
  as passes. Closing this needs a provisioned venv, which is a separate
  decision about whether CI may reach the network.
* **`nobuild` units are outside the gate.** Roughly 130 `stress/` fixtures are
  deliberately invalid (`must_fail/` and diagnostic repros). A post-emit parse
  gate (review item T0.1) is the right instrument for build-side correctness;
  this harness only compares two *executions*.
* **`stderr` is captured but not diffed.** VM tracebacks legitimately reference
  `.ty` source where CPython's reference `.py`, so a byte-diff would be pure
  noise. Only stdout and exit code gate. Divergences that are visible *only* on
  stderr are therefore not caught.
* **The knob matrix does not yet cover** `[checker] external = "ty"` (needs `ty`
  on `PATH`), `tyc build -O` as a *flag* (the `[optimise] level` config path is
  covered), or the `3.14`/`3.14t`/`3.15t` targets.
