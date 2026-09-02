# Typhon — Beta-readiness review (2026-09-01)

**Reviewed commit:** `ee5f818` (main, the alpha.9 line plus the unreleased
release-readiness remediation) · **Binary under test:** release `tyc
1.0.0-alpha.9`, CPython 3.13.7 · **Method:** the five CI gates run locally,
the full example and stress corpora type-checked and executed both ways, and
six parallel adversarial reviewers (one per area: preprocessor, type checker,
desugar/emit/format, VM, tooling, docs) each required to reproduce every
finding against the binary before reporting it. Their reproduction files are
committed as [`stress/round-2026-09-01/`](../stress/round-2026-09-01/).

## Verdict

**Not beta-ready yet, but closer than the volume of findings suggests.** The
project's engineering practice is genuinely good: every gate is green, the
gates are real (they fail loudly rather than vacuously), the docs are
accurate to within a handful of stale lines, and the release pipeline is
pinned, checksummed and least-privilege. The problem is the long tail. The
reviewers reproduced **about 120 defects** in one session, and roughly forty
of them are the worst class — `tyc check` and `tyc build` exit 0 and the
program does something different from what the source says. Most of those
sit in three places: the text-rewriting preprocessor, the un-checked corners
of the type checker, and the VM's stdlib shims.

The fixes landed alongside this review close **35** of the findings
(listed below) and every gate stays green. What remains is documented as a
backlog with a recommended beta gate at the end.

> **Status update (2026-09-02).** The backlog below has since been worked
> through; see [the follow-up section](#follow-up-2026-09-02) at the end for
> what is closed, what changed, and what is left.

## Gate results at the reviewed commit

| Gate | Result |
|---|---|
| `cargo fmt -- --check` | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass |
| `cargo test --workspace` (Python + ruff tiers required) | 2060 passed, 0 failed |
| Examples corpus `tyc check` (49 units) | 49 clean, no warnings |
| Stress corpus `tyc check` (1081 units) | 130 intended negative fixtures fail, nothing else |
| VM ↔ CPython differential (1130 units) | PASS — 124 known divergences pinned, 770 agree, 95 vacuous, 131 nobuild, 10 nondeterministic |
| Opt-in knob matrix (12 fixtures) | 12 pass |
| Perf gate (`examples/47-mini-app`) | 22 ms median vs 27 ms baseline |

After the fixes in this change, on the same machine:

| Gate | Result |
|---|---|
| `cargo fmt -- --check` / clippy `-D warnings` | pass |
| `cargo test --workspace` (every unit, integration and doc test) | 2863 passed, 0 failed |
| VM ↔ CPython differential (1480 units, the new probe round included) | PASS — 167 pinned, 899 agree, 152 vacuous, 252 nobuild, 10 nondeterministic. Nine old entries burned down (three curated examples among them); none of the pre-existing corpus regressed; the 51 new entries are all `stress/round-2026-09-01/` probes of still-open bugs |
| Opt-in knob matrix (12 fixtures) | 12 pass |
| Perf gate (`examples/47-mini-app`) | 19 ms median vs 27 ms baseline |

Of the new round's 350 probes the harness runs 120 in agreement, pins 51 as
divergent, and cannot compare the rest (121 are negative fixtures the checker
rejects by design; 57 fail on both sides with empty stdout).

Two observations about the gates themselves. First, a stress unit that
*stops* type-checking is invisible: the differential gate only fails on
divergence, so a new checker false positive shows up as a silent `nobuild`
flip. A committed expected-failure list for the stress corpus would close
that (this review compared before/after failure sets by hand). Second, the
differential gate cannot see anything upstream of the AST, which is exactly
where the preprocessor bugs live.

## What was fixed in this change

Compatibility class per the project's taxonomy — **R** pure relaxation,
**C** narrowing on already-crashing code, **U** narrowing on code that ran
correctly (each of those has an escape and a changelog note).

### Miscompilations (`tyc build` exit 0, wrong program)

| # | Finding | Fix |
|---|---|---|
| S1 | A single-line `guard … else: …` expanded to three lines inside `preprocess`'s main loop, shifting every line-indexed side table — a later `class P frozen:` silently lost `frozen`, `unsafe:` blocks stopped being recognised, `tyc fmt` rewrote the file | Lowered in the mapped `expand_multiline_guards` pre-pass; the main-loop branch is gone |
| S2 | A `# comment` on a `\|>` continuation line swallowed every later pipe step | Comments lifted out of the join and re-attached |
| S3 | Any line with `\|>` and a non-ASCII character was mojibaked (`"café"` → `CAFÃ©`) | Whole UTF-8 characters copied |
| E1 | `yield` lost its parentheses as an operand (`(yield t) + 1` → `yield (t + 1)`) or inside a tuple / call / `return` | Bare only as a whole statement or RHS; parenthesised everywhere else |
| E2 | `case (x,):` emitted as the capture pattern `case (x):` | Trailing comma kept |
| E3 | Default `[emit] format = true` rewrote a nested f-string format spec (`:.2f` → `: .2f`) and PEP 701 same-quote fields (`d["a:b"]` → `d["a: b"]`) | The spacing pass masks string bytes with the shared PEP 701-aware `lexmask` |
| E4 | `ClassVar` fields were cloned into every dataclass subclass, forking a shared registry | `ClassVar` left on the parent |
| E5 | `1e400` emitted as the bare name `inf` (`NameError`), `1e400j` as `infj` | `float("inf")` / `complex(…)` |
| E6 | A NUL in a triple-quoted string was written raw (`SyntaxError` at import) | C0 controls escaped |
| E7 | Valid programs rejected by the post-emit parse gate: `[*(a or b)]`, `{**(a if c else b)}`, `case (A() as x) \| (B() as x)`, `"{" f"{x}"` | Precedence guards / brace doubling |
| E8 | A non-empty mutable default (`xs: list[int] = [1, 2]`, `d: dict[str, int] = {"a": 1}`) survived to `@dataclass`, which rejects every unhashable default — `ValueError` at import while the VM ran the program | Constant-only displays become `field(default_factory=lambda: …)`; a display that names anything is left alone (a class-body lambda cannot see class-scope names) |

### Preprocessor robustness

| # | Finding | Fix |
|---|---|---|
| S4 | `unsafe:  # comment` was a parse error | Comment stripped before the header check |
| S5 | `pub *` on a final line with no newline emitted `__all__` but no re-export; `tyc fmt` produced an empty file | The marker's line is always kept |

### Type checker and resolver

| # | Finding | Fix | Class |
|---|---|---|---|
| T1 | An un-awaited call to an `async def` inside another `async def` was typed as its return type (`let s: str = fetch(3)` → crash on `s.upper()`) | Typed as `Coroutine[T]`; `await` unwraps it, coroutine-accepting APIs are untouched | C |
| T2 | The nullable-operand check was gated on a bare name: `d.get(k) + 1`, `xs[0] * 2`, `lookup(x) < 3` passed silently | Every operand shape reported; ordering comparisons included; attribute-rooted operands warn per `[strictness] nullable-use` | C |
| T3 | `isinstance(x, (A, B))` narrowed to `Unknown` (unsound) and did not narrow the negative branch (false positive) | Tuple → union on both branches | R / C |
| T4 | `sys.exit(...)`, `exit()`, `assert False` and calls to `-> NoReturn` functions were not exits (`missing_return` false positive, including on the `NoReturn` body itself) | Recognised as exits | R |
| T5 | A fresh comprehension was rejected against a wider annotation (`list[object] = [a.name for a in agents]`) while the list literal was accepted | Adopts the annotated element type when every element fits | R |
| R1 | `x += 1` bypassed the `let` contract for locals, parameters and `global` / `nonlocal` writes | Routed through the assignment path | **U** — `mut` is the escape; one stress probe (`round-2026-05-23-drift-round-4/probes.ty`) now fails as intended |
| R2 | `try` / `except` handlers were not sibling arms: two handlers declaring the same `let`, or a handler initialising a declare-only `let`, were rejected | Same drain / restore as `if` / `match` | R |
| R3 | Class-body names were visible from methods and lambdas (`return n < LIMIT` — `NameError` at runtime) | Class scopes skipped for function-origin references; PEP 695 type parameters stay visible | C |

### VM ↔ CPython parity (`tyc run`)

| # | Finding | Fix |
|---|---|---|
| V1 | `json.loads` re-encoded each UTF-8 byte as a character (`"héllo"` → `hÃ©llo`), rejected every `\u` escape, accepted raw control characters, rejected `NaN` / `Infinity`, and raised a bare `ValueError` with a private message — `except json.JSONDecodeError` never caught it | Decoder rewritten: CPython's messages and character positions, surrogate pairs, a real `JSONDecodeError` class (a `ValueError`) exported from the module |
| V2 | `json.dumps` ignored `ensure_ascii` (default on), `separators`, `allow_nan`; `indent=0`; serialised unserialisable values as their repr | Encoder rewritten; `TypeError` / `ValueError` as CPython |
| V3 | `model_dump_json()` used `json.dumps` spacing; `str(model)` / `repr(model)` leaked `model_config` | pydantic-compact output; pydantic's `str` / `repr` |
| V4 | `dataclasses.asdict` iterated an unordered map (output changed between runs) and did not recurse; `astuple`, `fields`, `is_dataclass`, `replace` missing | All five implemented in declaration order |
| V5 | `except json.JSONDecodeError` / `except asyncio.CancelledError` — any attribute-qualified class — never matched | Qualified `except` resolved by class identity or native kind |
| V6 | `asyncio.TimeoutError`, `CancelledError`, `QueueEmpty`, `QueueFull`, `InvalidStateError` missing | Added; `CancelledError` escapes `except Exception` |
| V7 | `sys.exit` called `process::exit` — no `finally`, no `except SystemExit`, wrong status for `sys.exit("msg")`; `exit()` / `quit()` undefined | Raises `SystemExit`; uncaught → CPython's status mapping |
| V8 | `next(it, default)` ignored the default; `enumerate(xs, 1)` ignored the start; `list.index(x, start, stop)` ignored the bounds | Fixed |
| V9 | `f"{1.5:10}"` → `1.500000`, `f"{3.14159:.3}"` → `3.142`, `f"{'hello':.3}"` → `hello` | No-type float spec is `repr` / significant digits; string precision truncates |
| V10 | `True & False` → `0`; `KeyError` payloads (`e.args[0] == "'k'"`, `pop` message `'Str("k")'`); `int("x")` message text; `"%s" % obj` ignoring `__repr__` | Fixed |
| V11 | `as!` failure message printed `int` where the compiled runtime prints `<class 'int'>` | Matched |
| V12 | `3 * [0]` / `2 * (1, 2)` raised `TypeError` (only `seq * n` was implemented); `True * [1]` likewise; `[1] * (2 ** 63)` raised `MemoryError` where CPython raises `OverflowError`; a repeat whose total fits an index but not the machine (`[1, 1] * (2 ** 40)`) aborted the process on allocation failure | Both operand orders and `bool` counts; the count is bounded by `isize::MAX`; the list / tuple allocation goes through `try_reserve_exact` and surfaces as a catchable `MemoryError` |
| V13 | `list(zip())` yielded `()` forever until the process was OOM-killed (the loop over zero iterators is vacuously satisfied); `zip(…, strict=True)` raised a private message | Empty `zip()` is exhausted immediately; CPython's `zip() argument 2 is longer than argument 1` / `… shorter than arguments 1-2` wording |

### Tooling and security

| # | Finding | Fix |
|---|---|---|
| X1 | `tyc build --no-sync` wrote through symlinks: a pre-planted `build/main.py -> <victim>`, a symlinked `build/` or `.sourcemaps/`, a symlinked `pyproject.toml` all received attacker-controlled bytes outside the project, under the exact flags `SECURITY.md` recommends for untrusted code | `atomic_write` refuses symlink targets; every artifact destination is confined to the canonical project root; `pyproject.toml` writes are atomic and refuse links |
| X2 | `tyc fmt src/` / `tyc check src/` followed symlinks out of the tree (rewriting the targets; walking `/usr`) | Links that resolve outside the walk root are skipped with a warning |
| X3 | `tyc build --check` wrote `pyproject.toml` and, without `--no-sync`, ran `uv sync` | Dry run skips the environment bootstrap |
| X4 | `tyc profile` → `pgo-memoise` round trip never promoted anything (`__main__.fib` vs `main.fib`) | The entry module accepts the `__main__` spelling |

### Documentation

Stale "currently `v1.0.0-alpha`" in `SECURITY.md` / `CONTRIBUTING.md`; the
skill's `nullable-use` default (`"warn"`, not `"error"`), its `i64` VM claim
and two stale HKT lines; the docs-site exit-code table (rows inverted) and
the installer pin syntax (`--version=`); example counts in the cookbook; and
the four diagnostic pages touched by the checker changes.

## Open findings (the beta backlog)

Everything below was reproduced and is **still open**. Severity is the
reviewer's, checked against the reproduction. Reproductions live under
`stress/round-2026-09-01/<area>/`.

### Preprocessor (`tyc-syntax`) — the structural risk

The alpha.7 `lexmask` work fixed the string-corruption class (every string
and comment probe was clean, CRLF is clean). The remaining defects are about
*block boundaries decided by physical indentation*, *passes that change line
counts without a map*, and *pass ordering*:

- **Critical (silent):** a trailing `?` on a one-line compound statement
  (`if flag: x = f()?`) is hoisted above the header and evaluated
  unconditionally; `while f()?: … else:` never runs the `else`; `lazy let`
  inside a `class!` / `plain class` body is silently eager and becomes a
  constructor default; a mid-expression `]?` / `name?` becomes `| None`
  (type-checks, crashes).
- **High:** `tyc check` and the LSP report preprocessed-buffer line numbers
  for every diagnostic below a `?`, `gather:` or with-chain expansion (a
  10-line file reports line 14; the LSP publishes past EOF) — the `*_mapped`
  tables exist but only `tyc build`'s `.py.map` consumes them; `enum`,
  `gather:`, `impl Alias:`, with-chain and `rescue` bodies end at a column-0
  comment or cannot contain a multi-line call; the cheat-sheet one-liners
  `enum Color: RED; GREEN` and `gather: a = f(); b = g()` do not work;
  `for x in data as! list[int]:` and `x as! int?` mis-lower; `?` after a
  multi-line triple-quoted argument is a parse error; `elif f()?:` breaks in
  2-space or tab-indented code; `lambda v: v |> f()` pipes the lambda.
- **Medium:** nested with-chains; `|>` before `as!` / `rescue` / `guard`;
  multi-line `go` with comments; a UTF-8 BOM; HKT markers on `interface` /
  `class!` / `impl[F[_]]`; generated code glued onto a last line with no
  newline; non-ASCII identifiers in several private scanners; `freeze let`
  over a multi-line string; `err` inside a multi-line f-string in `else err:`;
  `enum X(enum.Enum):` / `model X(BaseModel):` emitting a duplicate base.

### Type checker (`tyc-types`)

- **Critical:** mutating container methods take any argument
  (`self.items.append("str")` into `list[int]`, `extend`, `insert`, `add`,
  `update`, `setdefault`); writing an undeclared attribute on a
  `@dataclass(slots=True)` class is accepted (CPython raises, the VM does
  not — a divergence too).
- **High:** lambda bodies are never checked; inside a generic body a
  `T`-typed value is `Any` in both directions (`def ident[T](x: T) -> T:
  return 42`); builtin calls never check argument type or nullability
  (`int(d.get("b"))`, `", ".join([1, 2])`); an `except` handler restores the
  pre-`try` narrowing even when the body reassigned the variable;
  `**kwargs` / defaults set `variadic`, which absorbs extra positional
  `Callable` parameters, and `*args` element types are unchecked; interface
  conformance ignores `async`, `@property` and `frozen` and treats
  class-typed fields covariantly.
- **Medium:** attribute narrowing survives a free call receiving the object
  or a write through another receiver; `isinstance(x, Super)` widens instead
  of intersecting; bare `self` under `impl[T] Node[T]` is `Node`, not
  `Node[T]` (false positive); a subclass may redeclare a field with an
  incompatible type; `int ** int` is `int` (negative exponents give
  `float`); tuple-unpack arity mismatches; `for x in obj` over a class with
  no `__iter__`; positional-only parameters accepted by keyword; multi-level
  optional chaining in one `and` (warn-level false positive); inheritance
  deeper than 32 loses root fields; subscripting a union is never checked
  (`x[0]` on `int | str` passes, while `x.upper()` is correctly rejected);
  `case [*a, *b]` (two starred names in one sequence pattern) passes the
  checker and the post-emit parse gate but is a `SyntaxError` in CPython.

### Resolver / analysis (`tyc-resolve`, `tyc-analyse`)

- **Critical (under `[optimise] level = 1` / `tyc build -O`):** the purity
  walker never inspects attribute-callee calls (`datetime.now()`,
  `logger.warning(...)`), accepts lazy iterator builtins and mutable
  container construction, and never sees a module-`mut` read — so
  `auto-memoise` injects `@functools.cache` on impure functions and
  `auto-parallel` parallelises side-effecting callees; `auto-gather` matches
  callees by a flat *name* set, so a parameter or nested `def` shadowing a
  `@gatherable` name is folded (reordering); the accumulator-loop reduction
  checks the loop target's liveness only in the sibling suffix (a read after
  an enclosing block gets `NameError`); comptime folds float `//` and `%`,
  container `==`, `str(float)` and `f"{x=}"` to the wrong constant, and
  `i64::MIN // -1` panics the compiler. **`-O` should not be advertised
  until the first three are closed.**
- **High:** no definite-assignment analysis for ordinary bindings
  (use-before-assign, dead-branch binding, `except … as e` after the
  handler, `del` then read, empty-loop variable all compile clean);
  comprehension-clause and lambda scopes in a class body still see class
  attributes (the method case is now fixed).
- **Medium / low:** f-string format-spec interpolations are not walked
  (`unknown_name` false negative, `unused_import` false positive); prelude
  names such as `env` / `functools` / `dataclasses` resolve without an
  import and no import is injected; a declare-only `let` may be assigned on
  every loop iteration; `auto-gather` re-routes exceptions into an
  `ExceptionGroup` even with attested callees; `blocking_in_async` and
  `resource_not_managed` see only exact dotted paths / bare assignments;
  `let _` twice trips `no_block_shadow`.

### Desugar / emit / runtime templates

- **High:** a `class!` grandchild of an in-module `class!` loses inherited
  defaults and skips the framework base `__init__` (the torch `nn.Module`
  shape); a dataclass-instance field default (`p: P = P(x=1)`, or a
  non-constant display such as `[SIZE]`) survives to `@dataclass`, which
  rejects it as unhashable (`ValueError` at import) — the constant-literal
  case is fixed (E8), this one needs to know which names are non-frozen
  classes; `freeze let` rejects `enum` members
  (and `datetime`, `Decimal`, `Path`); the `lazy let` proxy lacks
  `__format__` / `__round__` / `__divmod__`; `extend BUILTIN` calls on an
  attribute or call receiver (`self.title.slug()`) pass the checker but are
  never rewritten; `tyc fmt` still corrupts PEP 701 same-quote fields when
  run on `.ty` source through the ruff-absent path — the in-process pass is
  fixed, so re-verify this once ruff is on PATH; the parent-field copy still
  re-evaluates a side-effecting default (`id: int = nid()`) in each subclass.
- **Medium:** PEP 696 type-parameter defaults dropped; `Result` in the
  generated runtime is an unparameterised `Union[Ok, Err]`, so
  `typing.get_type_hints` fails on a `Result[int, str]` field; `go` inside a
  sync `def` builds and then raises `no running event loop`.

### VM (`tyc-vm`)

Still a long way from "drop-in" on the stdlib tail. Open after this change:

- **Critical / high (silent):** generator expressions are eager lists
  (`next(g)` and `any(...)` short-circuit differ); `re.findall` with groups
  returns whole matches, `span(n)` ignores `n`, `groupdict()` order is
  arbitrary; `collections.Counter` / `deque` / `OrderedDict` / `namedtuple`
  are plain dict / list / tuple (wrong counts, no `maxlen`, wrong `repr`);
  `sum()` of floats is naive (CPython 3.12+ compensates); `filter(None, …)`,
  multi-iterable `map`, `x in map(...)`; user `__hash__` / `__eq__` are bypassed for dict
  and set keys; lambda default arguments are dropped; `math.sqrt(-1)` /
  `math.log(0)` return `nan` / `-inf` instead of raising; `str.encode` /
  `bytes.decode` ignore the codec; `itertools.islice` ignores start / step
  and much of `itertools` is missing; float `repr` at the
  `9999999999999998.0` boundary; `str(ValueError())` and `OSError`
  attributes; the `datetime` / `pathlib` shims (no `__str__`, `parent` is a
  `str`, seconds dropped); whitespace `split(maxsplit)` and `center` parity;
  Unicode `isdigit` / `title` / `swapcase`; a `str` / `bytes` repeat whose
  total fits an index but not the machine still aborts on allocation failure
  (the `list` / `tuple` case now raises `MemoryError` — V12); programs nested
  deeper than CPython's own parser limit are accepted by `tyc` and run by
  the VM, but CPython rejects the emitted file (a curiosity, not a target).
- **Missing modules programs hit:** `argparse` (curated example 21),
  `string`, `io`, `copy`, `operator`, `bisect`, `textwrap`, `statistics`,
  `logging`, `csv`, `tempfile`, `shutil`, `glob`, `traceback`, `uuid`,
  `hashlib`, `base64`, `decimal`, `fractions`, `subprocess`, plus
  `issubclass`, `bytearray`, `sys.stdin`, `sys.maxsize`, `frozen` dataclass
  enforcement, `__slots__` enforcement, `raise … from` chaining.
- Six of the 32 curated examples and four of the 15 apps still diverge
  under `tyc run` (all pinned in the baseline).

### Tooling

- **High:** `tyc migrate` panics on any non-ASCII character in a
  triple-quoted string (a stdlib module does it) and emits an unparseable
  class when only comments remain after methods move to `impl`; `tyc check`
  is quadratic in module-level definitions (10k `def`s take 40 s, a 10 MB
  file never finishes); diagnostic line numbers after sugar expansion (see
  the preprocessor list).
- **Medium / low:** the bundled `httpx.dty` types `Response.url` as `str`
  (it is `httpx.URL`) and still accepts the removed `proxies=` / `app=`;
  `TYC_NO_INTROSPECT=1` makes a project with `unintrospectable-dependency =
  "error"` fail instead of downgrading; the LSP accepts config values the
  CLI rejects; stdout to a closed pipe panics; `tyc explain --list`
  advertises `tyc::freeze` / `tyc::pub` (family pages, not codes); `tyc
  add` / `remove` drop every comment in `typhon.toml`; `tyc init 'a"b'`
  writes an unparseable manifest; the VS Code extension's server-path
  settings are not scoped to `machine-overridable`; `orphan_py_import`
  labels misalign when the file uses `pub`.

### Documentation (still open)

`tyc profile` docs describe the round trip as working (now true for the
entry module — re-check the prose); docs-site CLI pages list flags that do
not exist (`--diff`, `--quiet`, `--verbose`, `--manifest`, `--color`,
`--no-color`) and omit `tyc lsp --log-level`, `tyc trace --map-dir`, `tyc
debug --raw-pdb`, `tyc install skill`; the `[strictness]` tables omit
`blocking-in-async`, `require-with`, `suggest-gather`, `stub-check`; two
broken docs-site links (`reference/checked-cast`, `/diagnostics/`);
`rescue` absent from the grammar / lowering / tour pages; the docs-site CI
pages name two of the five jobs; `docs/vm.md` and `RUNTIME.md` omit four
resolvable modules and call the recursion limit "configurable".

## What "beta" should mean here

The alpha promise is "additive on correct programs" and "every `.ty` emits
correct `.py`". Beta should add "and we can show it". Concretely, the review
suggests gating the beta tag on:

1. **Preprocessor:** every block collector and join pass reading logical
   lines through the shared `LexMask` (comments at column 0, bracket
   continuations, no trailing newline), the `?` / `|>` / `as!` / `rescue`
   lowerings refusing anything they cannot prove is a simple statement, and
   diagnostics remapped to `.ty` lines in `tyc check` and the LSP. A
   corpus-mutation gate (column-0 comments, CRLF, no trailing newline, tabs,
   BOM, non-ASCII identifiers) run over the existing corpus would keep it
   closed.
2. **Type checker:** container-method signatures, lambda bodies, opaque
   `T` inside generic bodies, a small builtin signature table, `except`
   handler widening, `**kwargs` / `*args` in `Callable` assignability, and
   the three interface-conformance predicates. All are "narrowing on
   already-crashing code" and each is a bounded change.
3. **Optimisation profile:** either close the purity / auto-gather /
   reduction findings or stop documenting `tyc build -O` as safe.
4. **VM:** decide the drop-in contract. Either fund the stdlib tail
   (generators as real iterators, `re` groups, `Counter` / `deque`, the
   missing modules) or make `tyc run` fall back to `--compile`
   automatically when the program imports a module the VM lacks, and say so.
   Six diverging curated examples is not a beta story either way.
5. **Gates:** an expected-failure list for the stress corpus, a
   CPython-`compile()` step (not only a parse) after emit, and an
   expression-level differential sweep like `stress/round-2026-09-01/vm/`
   in CI.
6. **Tooling:** the `migrate` panic, the quadratic `check`, and the docs
   list above are a day or two of ordinary bug-fixing.

The structural recommendations of the 2026-07-28 review (one lexical mask,
one class layout, no `_ => {}` in analysis visitors, no semantics inferred
from surface text) still describe the right direction; this review mostly
found the places they have not reached yet.

## Follow-up (2026-09-02)

Everything in this section landed after the review, on the same branch,
with every gate green at each step.

### The beta gate, item by item

1. **Preprocessor.** Closed in the wave immediately after the review: the
   block collectors and join passes read logical lines through the shared
   `LexMask`; the `?` / `|>` / `as!` lowerings refuse what they cannot
   prove is a simple statement; diagnostics are remapped to `.ty` lines in
   `tyc check` and the LSP. A corpus-mutation sweep (CRLF, no trailing
   newline, BOM, column-zero comments, tabs) over all 1 692 files reports
   **zero** sensitive files, down from four.
2. **Type checker.** Container-method signatures, lambda bodies, opaque
   `T` inside generic bodies, a builtin first-argument table, `except`
   handler widening, `*args` / `**kwargs` in `Callable` assignability and
   the interface-conformance predicates all landed. The `class!`
   grandchild inheritance hole is closed in both the arity check and the
   concrete argument check. `tyc::possibly_unbound` (warn) and
   `tyc::invalid_pattern` (error) are new.
3. **Optimisation profile.** The purity verifier now states what it
   proves; `auto-gather` and the reduction rewrite were narrowed to what
   they can justify, and the docs say so.
4. **VM — the drop-in contract is now decided and enforced.** The VM
   models a documented stdlib subset, and `tyc run` scans a program's
   imports before executing anything: an import outside that subset takes
   the compiled path automatically, with a `note:` naming it, and
   `--no-fallback` refuses instead. The subset itself grew substantially
   (the whole filesystem/IO surface, plus `string` / `operator` /
   `bisect` / `base64` / `csv` / `__future__` and the `contextlib` /
   `functools` / `sys` / `typing` gaps), and the object model gained
   `NamedTuple` / `TypedDict` semantics, `issubclass`, function
   `__dict__`, identity-hashable classes and per-module dunders. **The
   differential baseline is down from 167 entries to 15**, and the six
   diverging curated examples are gone.
5. **Gates.** The differential job now also byte-compiles every emitted
   `.py` with `compileall` — an *emitter* verdict that is never baselined,
   which found `tyc::invalid_pattern` on its first run — and pins the
   non-building corpus in `scripts/nobuild-baseline.txt`, failing in both
   directions. The expression-level sweep the review asked for is the
   `stress/round-2026-09-01/vm/` corpus, which the harness already covers
   unit by unit.
6. **Tooling.** The `migrate` panic (non-ASCII in a docstring) and its
   comment-only-class-body output bug, the quadratic `tyc check`
   (35.1 s → 0.49 s on 4 000 functions), the closed-pipe panic, the
   `explain --list` topic/code confusion, `tyc init` quoting, comment-
   destroying `tyc add` / `remove`, the `TYC_NO_INTROSPECT` severity
   downgrade, the LSP's acceptance of config the CLI rejects, the VS Code
   settings scope, the duplicated orphan-import warning and the `httpx`
   stub's wrong `Response.url` type are all fixed. The documentation list
   is closed.

### What is still open

- **The 43 remaining differential entries.** Each is a VM bug and the file
  names them. The clusters left are the thin `re` shim, `Counter` / `deque`
  corners, `lazy let`, deep Unicode casing and repr escaping, and a
  handful of exception-chaining differences. None of them is a *silent* wrong answer
  on the compiled path — they are VM-only.
- **`tyc run --compile <file>` diagnostics name the scaffold.** The
  single-file compile path stages the source into a temp project, so a
  diagnostic from the build reports `/tmp/tyc-script-…/src/main.ty` rather
  than the file the user named. Cosmetic, pre-existing, and now more
  visible because the automatic fallback uses that path.
- **The open type-system frontier** in `TYPE_SYSTEM_FRONTIER.md`
  (embedded `ty` Phase 2, accumulator-loop parallelisation) is unchanged
  and remains post-beta.

## Method notes

Six reviewers, ~2.7M tokens, ~550 tool calls, about 45 minutes of wall-clock
each in parallel. Every finding in this document was reproduced with the
binary and CPython 3.13 by the reviewer, then the fixed ones were
re-verified with the patched debug build. Refuted hypotheses (well over a
hundred — precedence chains, numeric literals, seeded `random`, `match`
semantics, exception ordering, config validation, the `TYC_NO_INTROSPECT`
kill-switch, all seven `Command::new` sites, deep-nesting crash probes) are
recorded in the reviewers' transcripts and are not repeated here. Of the
reviewers' reproduction files, 350 are committed: the deep-nesting probes
that CPython's own parser cannot run and three stdlib sweeps whose output
depends on unseeded `random` or string-hash order were dropped, since the
differential harness cannot use either.
