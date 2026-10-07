# Differential and knob-coverage gates

The CI gates added in response to items **T0.2** and **T0.4** of
[`codebase-review-2026-07-28.md`](reviews/codebase-review-2026-07-28.md), plus the
de-formatted-corpus `tyc fmt` gate and the emitted-AST equivalence harness
(fourth review wave, W6/W7). All live in `scripts/`, run locally with no
network access, and refuse to run at all rather than run partially — a gate
that passes vacuously is worse than no gate, because it reads as coverage.

| Gate | Script | CI job | What it proves |
|---|---|---|---|
| VM ↔ CPython differential | `scripts/vm-differential.sh` | `differential`, `valid-corpus`, `differential-py315` | `tyc run` behaves identically to `tyc build` + CPython 3.13 over `examples/` + `stress/` (plus `corpus/valid/` under its own baseline — see below), and the emitted Python still runs under CPython 3.15 |
| Opt-in knob codegen matrix | `scripts/knob-matrix.sh` | `knob-matrix`, `knob-matrix-py315` | Every opt-in codegen knob actually fires, and does not change observable behaviour, under 3.13 and 3.15 |
| De-formatted corpus `tyc fmt` gate | `scripts/emitted-ast.py fmt-gate` | `fmt-corpus` | `tyc fmt` never changes what a program means, over every unit of `examples/` + `stress/` + `corpus/valid/` |
| Emitted-AST equivalence | `scripts/emitted-ast.py equiv` | — (run by hand) | Two compilers (binaries or git revisions) emit the same Python AST for every corpus unit, or exactly which units differ |

All require a release binary and a CPython **3.13+** interpreter reachable as
`python3.13`. The differential and knob scripts take another interpreter
through `PYTHON313` (the name predates the 3.15 legs); the `*-py315` CI jobs
run them with `PYTHON313=python3.15`:

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
| `nondeterministic` | not comparable: a side disagreed with **itself** across repeated runs (clocks, tempfile paths, task scheduling), or the unit is declared nondeterministic by construction | no |
| `both-timeout` | both sides exceeded the per-side limit | no |

Nondeterminism is detected only on the divergent path, by re-running *both*
sides twice more.

**That detection is probabilistic, and it is not sufficient on its own.** This
document previously claimed a clock-dependent program "can never flake the gate
red". That was wrong, and it cost several red runs on 2026-08-09 before the
mechanism was understood. The probe excludes a unit that disagrees with
*itself*; a unit whose output is coarse enough to repeat does not. A duration
printed at `.3f` seconds, or an unseeded `randint(0, 9)`, is frequently
identical across all three runs on each side — while the two sides still differ,
because the VM and CPython legitimately take different times and draw different
numbers. The probe clears it, and the harness reports a **new divergence** that
is not a VM bug at all. Finer-grained output (`.2f` milliseconds) self-disagrees
almost every time and is correctly excluded, which is why the failure looked
sporadic and unrelated to the change that surfaced it.

The fix is to stop relying on detection alone for those units — see
[declaring nondeterminism](#declaring-nondeterminism-by-construction).

After the parallel workers finish, the harness reconciles the number of
classified results against the number of selected units and exits `2` on a
mismatch (or a non-zero `xargs` status): a worker that crashes without
printing would otherwise silently drop its unit from *every* category, which
could hide a divergence or mis-report a baseline entry as fixed.

### Environment sensitivity

Each unit's classification depends on which third-party packages the ambient
`python3.13` can import: a unit whose emitted Python does `from pydantic
import BaseModel` (every `model` class) runs to completion where pydantic is
installed but dies at the import — usually into the `vacuous` bucket, or into
`diverge` when the VM's native shim succeeds where CPython's import fails —
where it is not. **The baseline is therefore only meaningful relative to a
package set**, which is recorded in its header comment (currently: `pydantic`,
`PyYAML`). The CI differential job installs pinned versions of exactly that
set, and `--update` runs must be performed with the same set importable, or
the recorded classifications will not reproduce anywhere else.

### The expectations file

`scripts/differential-baseline.txt` lists the unit ids that are known to
diverge, one per line, `#` for comments. The gate fails when:

* a unit diverges and is **not** listed — a regression; **and**
* a listed unit produces a genuinely comparable, non-divergent run (class
  `ok`) — a stale entry.

Failing in both directions is deliberate: the baseline can only shrink, never
rot. **Every line in it is a VM bug.** It is a burn-down list, not an
allow-list.

A listed unit whose run was **not comparable** in the current environment
(`vacuous`, `nobuild`, `noentry`, `nondeterministic`, `both-timeout` —
typically because a package from the baseline's recorded set is missing) is
reported as **"unverifiable here"** with its classification, and does *not*
fail the gate: it is neither fixed nor regressed, and failing on it would make
the gate red on any machine whose site-packages differ from the recording
environment's.

A `--scope` / `--filter` run only compares against the slice of the baseline it
actually covered, so a partial run can flag a regression but can never report
the uncovered remainder as "fixed".

### Declaring nondeterminism by construction

`scripts/differential-nondeterministic.txt` lists units whose stdout cannot be
compared between the two surfaces at all, because it contains a wall-clock
duration, an unseeded random draw, an address-dependent repr, or a
task-completion order. They are classified `nondeterministic` and excluded from
the verdict, without depending on the probabilistic probe above to notice.

**This is not the baseline, and the distinction matters.** A baseline line means
"known VM bug, burn it down"; the file nags you to delete entries. A declaration
means "not a bug, and not comparable". Recording a timing program as a VM
divergence would be recording a false claim, and would permanently retire the
unit's coverage behind a label asserting something untrue about the VM.

**Prefer fixing the unit to listing it.** If the nondeterministic value is not
what the unit is testing, print something stable and keep the real coverage:

```ty
print(f"{name}: {elapsed:.3f}s")            # not comparable
print(f"{name}: measured={elapsed >= 0.0}") # comparable, still proves __exit__ ran
```

Two stress units were fixed that way rather than listed, which moved them from
*excluded* to real differential coverage. Only declare when the nondeterministic
value **is** the point of the program — as in
`examples/58-context-managers`, where printing an elapsed time is the
demonstration, and neutering it to satisfy a harness would be the tail wagging
the dog.

The gate keeps the list honest from both ends, but asymmetrically:

* **Hard fail** — an entry naming a path that no longer exists, or an entry that
  also appears in the baseline (a unit cannot be both a VM bug and not
  comparable). Both checks read the filesystem and the baseline rather than this
  run's results, so they are deterministic and can never flake.
* **Warn only** — a listed unit that comes back fully reproducible *and*
  agreeing on both sides. That is evidence the entry is stale, but not proof: a
  genuinely nondeterministic unit agrees by chance now and then, so failing on a
  single observation would reintroduce exactly the flakiness this file exists to
  remove.

### Running it

```bash
scripts/vm-differential.sh                       # whole corpus, gate against the baseline
scripts/vm-differential.sh --scope examples      # examples/ only
scripts/vm-differential.sh --scope valid \       # corpus/valid/ against its own baseline
    --baseline scripts/valid-corpus-baseline.txt
scripts/vm-differential.sh --jobs 16             # parallelism (default: nproc)
scripts/vm-differential.sh --report r.tsv        # full per-unit TSV
scripts/vm-differential.sh --update              # rewrite the baseline from this run
PYTHON313=python3.15 scripts/vm-differential.sh \  # the CPython 3.15 leg
    --extra-baseline scripts/differential-baseline-py315.txt
```

**The CPython 3.15 leg.** The corpus is built for the default 3.13 target and
run under CPython 3.15 as well, so a 3.15 interpreter or stdlib change that
breaks emitted code fails CI (`differential-py315`). The VM reproduces 3.13,
the minimum supported target, so units whose stdout changed in CPython itself
(3.14/3.15 error-message wording, compensated `sum` over mixed int/float,
pathlib treating a trailing `.` as a suffix) are listed in
`scripts/differential-baseline-py315.txt`. `--extra-baseline` unions that file
with the main baseline instead of copying it; the both-directions rule applies
to its entries too, and `--update` refuses to run with it.

Triaging one entry:

```bash
TMPDIR=/tmp/triage scripts/vm-differential.sh \
    --filter 'examples/57-iterators-generators' --keep
# then diff cpy.out / vm.out and read vm.err in the kept workdir
```

### The valid-programs corpus (`--scope valid`, W6-18)

`corpus/valid/` holds real, ordinary libraries (seeded from CPython 3.13's
stdlib via `tyc migrate`, hand-fixed once — see `corpus/valid/README.md`),
kept green by the `valid-corpus` CI job: `tyc check corpus/valid/` (every new
diagnostic is swept over ordinary code, so a false positive like W1-01/W1-02
fails fast) plus the same harness over `--scope valid`. The valid scope has
its **own** baseline file, so the two gates evolve independently — and `all`
stays `examples` + `stress`, so the main baseline never sees valid units.

The valid corpus is stdlib-only by design, so unlike the main baseline it has
no third-party package set to keep in sync. Its seed triage (2026-10-03,
alpha.9) is recorded in `corpus/valid/README.md`: four units agree
byte-for-byte, `heapq` agrees including its doctests, `textwrap` is a
baselined VM bug (`dict.fromkeys` unmodelled), and `string.ty` is
`vacuous-runtime` on both surfaces (an emitter bug in explicit
`__init_subclass__()` calls — out of scope for the corpus, which stays
faithful to upstream).

**Do not `--update` a custom `--baseline`.** `--update` rewrites `$BASELINE`
(which honours `--baseline`) **and** the shared
`scripts/nobuild-baseline.txt` (which does not) from *this run's* units, so an
`--update --scope valid` would truncate the main nobuild list to the valid
units. Grow `scripts/valid-corpus-baseline.txt` by hand from a `--report` run
after triaging each entry; all seven seed units build, so nothing valid
belongs in the nobuild list today.

Runtime is roughly **75 s** for the full 1130-unit corpus at `--jobs 8` on a
4-core machine; CI runs it at `--jobs 2`. CI used `--jobs 4` until v1.0.0-alpha.9:
four concurrent `tyc` + `python3.13` pairs took a 16 GB hosted runner to
~4.8 GB available with swap 99.7% exhausted by the end of the harness, and the
job was intermittently killed with `The runner has received a shutdown signal`
(exit 143) partway through. Halving the worker count halves that peak — but it
did **not** stop the kills, which continue at `--jobs 2` and remain unexplained
(see the comment on the job in `.github/workflows/ci.yml` for the hypotheses
ruled out so far). Re-running the job clears it. Raise the worker count
locally if you have the headroom — the harness is CPU-bound below the memory
ceiling, so more workers are strictly faster until you hit it.

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
| `meta.conf` | `run=both\|none`, `expect-exit=N`, `vm-diverges=yes`, `requires-module=NAME`, `min-python=3.N` |

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
| `lazy-import-pep810` | `[python] target = "3.15"` | native `lazy import json as js` instead of the runtime proxy (`min-python=3.15`: build-only under python3.13, executed by `knob-matrix-py315`) |

### Running it

```bash
scripts/knob-matrix.sh
scripts/knob-matrix.sh --filter auto-parallel
scripts/knob-matrix.sh --filter pgo --keep     # keep both builds for inspection
```

Runtime is a few seconds. Adding a knob means adding a directory — no script
change.

A fixture whose emitted Python needs a newer interpreter declares it with
`min-python=3.N` in `meta.conf`. Under an older `PYTHON313` its execution half
is skipped by design and printed as `ok (build-only: needs Python 3.N+)`; unlike
a missing `requires-module`, that is not a reduction and does not fail under
`TYC_REQUIRE_PYTHON`, because `knob-matrix-py315` executes it:

```bash
PYTHON313=python3.15 scripts/knob-matrix.sh
```

### Missing runtime dependencies

A fixture may declare `requires-module=NAME` when its *execution* half needs a
third-party package (its codegen half never does). If the module is missing the
fixture drops to **build-only** and says so on its own result line *and* again
in the summary as `REDUCED to build-only`. It is never skipped silently: a skip
nothing observes is indistinguishable from a pass. CI installs `pydantic` so
the matrix runs at full coverage there; locally the script itself stays
network-free.

---

## 3. De-formatted corpus `tyc fmt` gate

### Why

`tyc fmt` rewrites files in place and is the one tool people run without
reading the diff. It has corrupted source in four release lines (`?` turned
into `| None`, string-literal contents respaced, …), each time through a text
edit that a parse of its own output would have caught. Since W7-03 the
formatter lowers its output as it lowered its input, parses both, and refuses
to write unless the two ASTs are equal (`tyc_syntax::ast_equiv`). This gate
checks the same property end to end, over the whole corpus, through
`tyc build`.

### What it does

1. Build every unit of `examples/`, `stress/` and `corpus/valid/` as written
   and record the AST of every emitted `.py` (`ast.dump`, positions and
   comments ignored, compiler temporaries named `__typhon_*` renumbered by
   first appearance, docstrings compared with each line trimmed because
   `ruff format` may re-indent them).
2. Copy the corpus and de-format the code in every `.ty` file: `let x: T = 1`
   becomes `let x:T=1`, spaces after `,` and `:` and around `=`, augmented
   assignments and `->` are removed, and two trailing blanks are added to
   code lines. Indentation, line breaks, strings and comments are untouched,
   so the program means the same thing.
3. Run `tyc fmt` on each de-formatted file (with `ruff` on `PATH` in CI, so
   the whole formatter runs, not only its whitespace pass).
4. Build the formatted copy and compare every unit's emitted AST with step 1.

The gate fails when a unit's emitted AST changes, when a unit builds on one
side only, or when `tyc fmt` refuses a de-formatted file it formats fine as
written. A file `tyc fmt` cannot format even as written (a parse-error probe
in `stress/`) is reported as an expected refusal. There is no baseline file:
the first run on the current corpus was clean (1,224 built units identical,
264 non-building units non-building on both sides, 8 expected refusals).

### Running it

```bash
python3.13 scripts/emitted-ast.py fmt-gate --tyc tyc/target/release/tyc
python3.13 scripts/emitted-ast.py fmt-gate --scope examples --filter '^examples/0' --keep
```

`--report PATH` writes one TSV line per unit (`same`, `changed`, `status`,
`same-nobuild`, …). `--keep` keeps the scratch tree, including the
de-formatted and formatted copies. On this Mac a full run takes a few
minutes at `--jobs 8`.

---

## 4. Emitted-AST equivalence harness

### Why

Step 0 of [`design/sugar-as-ast-nodes.md`](design/sugar-as-ast-nodes.md): before
a lowering moves from text rewriting to AST nodes (or any other refactor that
should not change a single emitted program), there has to be a way to prove
it. The harness builds every corpus unit with two compilers and compares the
Python they emit.

### Running it

```bash
# Two git revisions: each is exported with `git archive` and built with
# `cargo build --release --bin tyc` (target dir: tyc/target/emitted-ast).
python3.13 scripts/emitted-ast.py equiv origin/main HEAD

# Two binaries you already have:
python3.13 scripts/emitted-ast.py equiv /tmp/tyc-before tyc/target/release/tyc

# Narrow it while iterating:
python3.13 scripts/emitted-ast.py equiv origin/main HEAD --scope examples --filter apps/
```

The corpus always comes from the working tree, so only the compiler differs
between the two sides; the working tree and the repository's refs are never
modified. The comparison uses the same normalisation as the fmt gate, without
the docstring leniency unless `--lenient-docstrings` is given. The report
lists each unit whose emitted AST changed, with the first differing line of
the two `ast.dump`s, and each unit that builds on one side only. Exit status
is 0 when every unit agrees and 1 otherwise, so it can gate a refactor's PR.

`dump` and `compare` are the building blocks, for when the two compilers are
not available at the same time:

```bash
python3.13 scripts/emitted-ast.py dump --tyc /tmp/tyc-before --out /tmp/ast-before
python3.13 scripts/emitted-ast.py dump --tyc tyc/target/release/tyc --out /tmp/ast-after
python3.13 scripts/emitted-ast.py compare /tmp/ast-before /tmp/ast-after
```

---

## 5. Known gaps

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
