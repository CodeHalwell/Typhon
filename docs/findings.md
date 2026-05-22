# Typhon — Findings

This is the single source of truth for stress-test findings across every
campaign that has run against `tyc` so far. It supersedes (and replaces):

- the old top-level `FINDINGS.md` (campaigns through 2026-05-20),
- `EXAMPLES_REVIEW.md` (the 47-example sweep that produced findings F1–F5),
- `docs/follow-ups-2026-05-17.md` (the May 2026 completion sprint),
- `stress/round-2026-05-20/FINDINGS.md` (R3.1–R3.18),
- `stress/round-2026-05-20-exploration/FINDINGS.md` (E1–E11),
- `stress/round-2026-05-21/FINDINGS.md` (B1–B14).

The original `stress/round-*/` test corpora are kept on disk under
`stress/` so each finding's repro file can still be replayed; only the
write-ups have been consolidated here.

---

## Quick status

**Ten stress-test campaigns** have been run between May 17 and May 22,
2026. Across them ~700 hand-written `.ty` programs were authored
spanning language edge cases, IO, ML/numpy, mock LLM/RAG, agents, APIs,
SDK patterns, perf, and intentionally-broken diagnostic probes. Roughly
**130 distinct findings** were filed.

| Round | Branch | New findings | Verdict |
|---|---|---|---|
| 2026-05-17 sprint | `claude/assess-project-completion-gUdOP` | sprint follow-ups (see [Sprint history](#sprint-history)) | mostly closed |
| 2026-05-18 #1 | `claude/test-typhon-library-EnZmE` | #1–#36 | all closed except #18 |
| 2026-05-18 fix-up sweep | `claude/update-findings-IdfrH`, `claude/fix-findings-diagnostics-hYj8K` | — | closed 33 of 36 |
| 2026-05-18 #2 | `claude/test-typhon-library-kxGRX` | #37–#56 | all closed in `resolve-open-findings-d6EIV` |
| 2026-05-18 examples sweep | `claude/review-examples-x5kgr` | F1–F5 (cross-cutting), 7 compiler bugs | compiler bugs closed; F1–F4 still open (see below) |
| 2026-05-19 | `claude/test-typhon-library-rNIYC` | #57–#127 | all but the deferred items closed in `review-findings-fixes-VRFJy` / `resolve-open-findings-UDZfv` |
| 2026-05-20 | `claude/test-typhon-library-ejNr5` | R3.1–R3.18 | closed in `resolve-open-findings-v6t65` except `tyc fmt` and `?` in sub-expr |
| 2026-05-20 exploration | `claude/typhon-exploration-testing-LZezp` | E1–E11 | closed except E6, E9, E11 |
| 2026-05-21 | `claude/tender-hawking-LLhuR` | B1–B14 | closed B1–B8, B10–B14; only B9 (`tyc fmt`) still open |
| 2026-05-21 findings sweep | `claude/findings-documentation-review-HhuVH` | — | closed O2/O3/O4/O5/O6/O10/O21/O22/O23/O24/O25/O26; verified-fixed O1/O7/O8/O9/O11/O13/O18/O20 |
| 2026-05-21 follow-up sweep | `claude/finish-open-findings-dmLv6` | — | closed O12/O14/O15/O16/O17/O27/O28/O29; verified-fixed O19 |
| 2026-05-22 | `claude/typhon-library-testing-ch1yz` | N1–N13 (3 CRITICAL) | all 13 closed before merge (shipped in v0.3.1) |

**Pass rate trend** on the canonical example suite (`examples/01-…46-…`):
20/47 → 39/47 → 46/46 → 46/46. The examples now build and run end-to-end
on every commit; new findings come from the stress corpora rather than
the curated examples.

---

## Open findings

None. Every finding filed across the ten stress campaigns is now
either closed or verified-fixed against its repro. The
"Recently closed" sections below carry the write-ups of the latest
sweeps, and [Sprint history](#sprint-history) records the per-branch
rollup.

Severity legend: **CRITICAL** (silent wrong output / runtime crash on
documented happy path), **HIGH** (documented feature unusable),
**MEDIUM** (feature works with workaround), **LOW** (UX / DX nit /
docs).

---

## Recently closed: 2026-05-22 stress round (N1–N13)

Branch: `claude/typhon-library-testing-ch1yz`. Released as
[v0.3.1](https://github.com/CodeHalwell/Typhon/releases/tag/v0.3.1).
A second stress campaign (93 fresh `.ty` programs across nine domain
buckets) filed thirteen new findings — three CRITICAL silent-wrong-
output, five HIGH, two MEDIUM, three LOW. Every one closed in this
release.

**CRITICAL closures (silent wrong output):**

- **N5 / N6** (`9c423b9`) — `Expr::UnaryOp` printer arm now consults
  `expr_precedence` on the operand and wraps when the operand's
  precedence is lower than `Not`'s. Without the fix, `not (a or b)`
  round-tripped as `not a or b` (De Morgan violation) and
  `not (x if c else y)` as `not x if c else y` (associativity flip).
  Surfaced organically against `05-agents/01-react-agent.ty` — a
  calculator-tool guard
  `not (ch.isdigit() or ch in " +-*/()")` was being emitted wrong.
- **N9** (`a68d1ec`) — VM `match` arm body now shares the enclosing
  env. The arm body used to push a fresh frame, so writes to
  outer-scoped bindings inside `case Ok(v): total = v` were
  discarded on arm exit; only the pattern's introduced names
  (`v`) stay scoped to the arm. Every accumulator / state machine /
  Result walker run via `tyc run` (default) used to produce a
  different answer from `tyc run --compile`.

**HIGH closures:**

- **N1** (`a5b6842`) — `freeze let` multi-line RHS. The
  `__typhon_freeze__(...)` wrap is now AST-level rather than a
  text-level fix-up on the binding's first line, so a multi-line
  dict / list literal no longer leaks out of the synthesised call.
- **N2** (`89b4685`) — `?` inside a comprehension is now rejected
  with a targeted diagnostic ("`?` cannot be lifted out of a
  comprehension — rebind the result and unwrap it"). The previous
  behaviour silently hoisted past the `for`-binding into a top-level
  `try`/early-return.
- **N10** (`6546184`) — `tyc run` now runs a static check before
  the VM, so unresolved names surface as `tyc::unknown_name`
  instead of a Python-style `NameError` at VM time.
- **N11** (`4e17492`) — `tyc migrate` rewrites
  `class X(Generic[T]):` → `class X[T]:` and drops
  `T = TypeVar("T")` / `from typing import TypeVar, Generic`
  declarations. Output now passes a clean `tyc build` on pre-3.12
  generic idioms.
- **N13** (`1069fec`) — `match self.<field>:` exhaustiveness.
  `match_subject_type` now delegates `Expr::Attribute` subjects to
  `infer_expr_readonly`, so a class with a sealed-union field can
  match it directly without binding to a local. Non-exhaustive
  variants still surface both `tyc::non_exhaustive_match` and
  `tyc::missing_return`.

**MEDIUM closures:**

- **N4** (`c4214e3`) — typed tuple-unpacking `let`.
  `let (a: int, b: str) = func(x, y)` parses and desugars to a
  hidden `__typhon_unpack_N__` temp + per-element `let`s carrying
  user-supplied annotations. Compound annotations (`list[int]`,
  `tuple[float, ...]`), mixed captures (`let (a: int, b) =
  pair()`), and the existing un-annotated form all covered.
- **N8** (`033adc7`) — new `tyc::duplicate_method` diagnostic. Two
  `impl Foo:` / `extend Foo:` blocks both defining `def get(self)`
  used to merge silently with Python keeping the last one. Now
  anchored at the second `def` with rename / delete / merge advice.

**LOW closures:**

- **N3** (`f3c6863`) — comptime `str.join(...)` joins the existing
  comptime sandbox alongside `str` / `int` / `float` / arithmetic /
  `env` surface.
- **N7** (`b73b634`) — newtype boundary mismatches now route
  through `tyc::newtype_violation` (with help text pointing at the
  constructor call) instead of `tyc::type_mismatch` (whose
  wrong-direction advice told users to widen rather than wrap).
- **N12** (`868fe63`) — a user-authored
  `from __future__ import annotations` is no longer duplicated by
  the emit pass.

---

## Closed in previous branch (claude/finish-open-findings-dmLv6)

Branch: `claude/finish-open-findings-dmLv6`. Closed every Open finding
remaining at the start of the branch: O12, O14, O15, O16, O17, O27,
O28, O29; plus verified-fixed O19.

**Code-fix closures:**

- **O12** — `tyc fmt` in-process pass now applies five PEP 8 rules
  beyond the existing whitespace pipeline:
    * space after `:` outside slice context (`x:int` → `x: int`),
    * spaces around `->` (`)->int:` → `) -> int:`),
    * space after a missing `,` (`f(x,y)` → `f(x, y)`),
    * spaces around top-level `=` (kwargs / `+=` / `==` left tight),
    * single-space around binary `+` / `-` (unary left tight), and
    * two blank lines before top-level `def`/`class`/`async def`.
  The O12 repro `def    f(  x:int,y:int)->int:` + `let z:int=x+y`
  now reformats end-to-end to `def f(x: int, y: int) -> int:` +
  `let z: int = x + y`.
- **O14** — new `tyc::unsafe_value_leak` diagnostic enforces Rule 5.
  `TypeBinding` carries a `from_unsafe` flag plus a long-lived
  `unsafe_origin_bindings` map (the env-scope restore that follows
  the `if True:` body would otherwise drop the binding). A
  `return x` outside the block where `x` was declared inside
  `unsafe:` and the function's annotated return is concrete now
  fires the diagnostic with help text pointing at both workaround
  forms (`let x: T = …` inside, or `let typed: T = x` outside).
- **O15** — TypedDict-style dict literal `let alice: User = {"id":
  1, "name": "Alice"}` now type-checks. When the expected type is a
  registered class shape, `Expr::Dict` matches keys against fields
  and each value flows under its declared field type before falling
  through to the ordinary `dict[K, V]` inference path.
- **O16** — built-in containers (`list`, `dict`, `tuple`, `set`,
  `str`, `bytes`, `range`, `frozenset`, `bytearray`) now satisfy a
  user-declared Protocol whose declared methods are all common
  dunders (`__len__`, `__iter__`, `__getitem__`, `__contains__`,
  `__eq__`, `__bool__`, etc.). `take([1])` against
  `def take(s: Sized) -> int` now type-checks across every built-in
  container shape.
- **O17** — new `expand_inline_question_ops` pre-pass lifts every
  `)?` not at the line-end position into a `__typhon_qi_N__` temp +
  propagation guard, then the existing end-of-line pass handles
  what remains. `Ok(add(parse(s)?, parse(t)?))` now compiles. The
  `tyc::invalid_question_op` diagnostic is repurposed to enforce
  the same Result-return scope rule the end-of-line case carried.
- **O27** — `Emitter::set_source()` lets the printer peek at a
  node's original `TextRange` to recover the bracket choice on
  `MatchSequence` patterns. `case [a, b]:` now re-emits as
  `[a, b]` rather than the default `(a, b)`. `tyc build` threads
  the preprocessed source through automatically.
- **O28** — pipe rewriter now accepts:
    * `5 |> (lambda x: x * 2)()` — `apply_pipe_call` walks back from
      the trailing `)` to find the matching `(` (rather than taking
      the first `(`), so a parenthesised callable head is
      recognised.
    * `(1 |> add(2)) |> add(3)` — new
      `expand_pipes_in_subexpressions` pre-pass recursively expands
      pipes inside every balanced `(...)` group before the line-level
      pass runs, so inner pipes at depth 1+ no longer leak through
      as parser errors.
  The `s |> S.add(5)` class-name dispatch variant was verified-fixed
  against the same machinery — it fell out for free when the
  self-counting arity check from R3.11 saw the receiver via the
  positional-arg slot.
- **O29** — new `--no-sync` flag on `tyc build` (and
  `TYC_NO_SYNC=1` env var) skips the `uv sync` step while still
  merging `pyproject.toml`. Stress harnesses and REPL-like
  iteration on tmp projects no longer pay the per-invocation
  reprovision cost.

**Verified-already-fixed against their repros:**

- **O19** — sealed-union exhaustiveness for the three legacy
  patterns (recursive sealed types with positional captures,
  multi-variant dataclass enums with empty variants, three-variant
  tool unions with positional captures) all type-check end-to-end
  now. Repros at `01-syntax-edge/05-sealed-recursive.ty`,
  `05-agents/03-agent-state-machine.ty`,
  `04-ai-llm/03-llm-tools-sealed.ty` all pass `tyc check`
  cleanly; the non-exhaustive form still produces
  `tyc::non_exhaustive_match` + `tyc::missing_return` as expected.

---

## Closed in previous branch (claude/findings-documentation-review-HhuVH)

**Code-fix closures:**

- **O2** — `Stmt::While` now applies test-implied narrowings to the
  body via the same `collect_narrowings` / `apply_narrowings` path
  the `if` checker uses; the linked-list iterator idiom
  `while cur is not None: total += cur.value; cur = cur.next`
  type-checks. Reassignment inside the body still resets narrowing
  at the assignment site, so post-mutation reads continue to trip
  `nullable_use` correctly.
- **O3** — `tuple[T, ...]` resolves to an internal `tuple_variadic[T]`
  head (displayed back as `tuple[T, ...]`) that the unifier accepts
  against any fixed-length tuple literal whose elements are all
  assignable to `T`, including `()`. Element-type hint also
  propagates into the literal slots so int literals widen to float.
- **O4** — Cyclic type aliases still surface
  `tyc::cyclic_type_alias` once, but the alias body is now rewritten
  to `Any` so subsequent uses fall through silently instead of
  cascading into `type_mismatch` errors. The diagnostic help text
  now names the recursive-container shape so users know what's
  actually unsupported and what to reach for as a workaround.
- **O5** — `class! Foo(Exception): pass` now synthesises
  `def __init__(self, *args, **kwargs) -> None: super().__init__(*args, **kwargs)`
  when the body has no annotated fields, so `raise AppError("boom")`
  reaches the parent constructor. The class-with-fields path is
  unchanged.
- **O6** — `return` inside a generator function body is now
  accepted against the declared `Iterator[T]` / `Generator[Y, S, R]`
  return type (both bare `return` and `return value` forms). The
  checker tracks an `in_generator` flag set from `body_has_yield`
  and the return-statement validator skips its usual assignability
  check while it's on.
- **O21** — `tyc migrate` now puts the `?` *inside* the forward-ref
  quotes: `Optional["Item"]` becomes `"Item?"`, not the previously
  unparseable `"Item"?`.
- **O22** — `tyc migrate` now rewrites `Union[T, None]` (and
  `Union[None, T]`, including the `typing.Union[...]` qualified
  form) to `T?`, drops the `typing.Union` import, and falls back
  to a PEP 604 pipe-union for multi-arm unions like
  `Union[A, B, None]` so the import is never left dangling.
- **O25** — `tyc explain --list` now prints every diagnostic code
  the binary knows about. The "not yet implemented" message is
  gone from the unknown-code error too.
- **O10** — `case Wrap(value):` against an outer `let value` now
  fires `tyc::pattern_shadows_outer` (a new diagnostic) instead of
  the misleading `tyc::immutable_assign`. The new help text
  suggests renaming the capture — the right advice for the
  Rust/OCaml/Scala intuition every newcomer brings to `match` —
  rather than the wrong `change \`let\` to \`mut\`` hint. The
  resolver tracks an `in_pattern` counter around each case pattern
  walk and consults it when deciding which diagnostic to push.
  Python pattern semantics are unchanged; only the surface message
  is new.
- **O26** — `tyc repl` now checks `stdin.is_terminal()` and skips
  the `>>> ` / `... ` prompts when stdin is piped. Interactive
  sessions on a TTY are unchanged; scripted input no longer
  produces the `>>> >>> >>> 6` shape the finding reported.
- **O24** — The VM's `Value::ResultOk` / `Value::ResultErr` repr now
  matches the CPython dataclass default
  (`Ok(value=20)` / `Err(error='oops')`), and string repr prefers
  single quotes the same way Python's `repr` does. `tyc run` and
  `tyc run --compile` produce byte-identical stdout for Result-
  bearing programs.
- **O23** — `extend list[int]:` (parametric target) now fires a
  dedicated `tyc::extend_builtin` diagnostic that names the
  parametric shape and tells the user to drop the `[…]` (or wait
  for the per-element-type dispatch the rewriter is tracked to
  gain). The previous behaviour was a confusing downstream
  `tyc::impl_unknown_class` cascade from the silently-stripped
  `__typhon_impl_list` pseudo-class.

**Verified-already-fixed against their repros:**

- **O1** — paren-stripping; every shape in
  `08-meta-stress/25-paren-emit-suite.ty` and
  `03-paren-wrong-output.ty` now round-trips correctly.
- **O7** — `impl X:` methods accept `T?` parameters.
- **O8** — `return self` from `impl __enter__` type-checks (the
  `__typhon_impl_*` pseudo-class no longer leaks into the receiver
  type — the type-checker strips the prefix when reading `self`'s
  receiver class).
- **O9** — `type Handler = Callable[[Req], Resp]` is transparent
  on call.
- **O11** — for-target rebind across loops in the same scope works.
- **O13** — pydantic is auto-injected into the synthesised
  pyproject.toml even when `typhon.toml` lacks an explicit
  `[dependencies]` section. Re-ran every `model`-using fixture in
  `stress/round-2026-05-21/` (06-api/{01,02,03}.ty,
  04-ai-llm/{01,04}.ty) end-to-end through `uv sync` and CPython;
  all five build and run cleanly. The original
  "bootstrapping-path-dependent" failure isn't reproducible
  against the current code.
- **O18** — `__mul__(self, scalar: float)` resolves correctly
  against `a * 5.0`.
- **O20** — comptime f-strings evaluate end-to-end
  (`comptime let VERSION: str = f"{APP} v{MAJOR}.{MINOR}"`).

---

## Sprint history

For full historical detail of how each closed finding landed, see the
git history of the per-branch commits referenced below. This section
records *what* shipped in each branch; the individual finding write-ups
have been retired now that they're closed.

### May 17 — completion sprint (`assess-project-completion-gUdOP`)

Landed: module-level + class-level `lazy let` lowering; `extend BUILTIN:`
implementation; `unsafe:` block depth tracking; PEP 695 typevar
substitution; `tyc trace`; `tyc profile`; `tyc migrate`; LSP hover /
go-to-definition; `tyc check --stubs`. 514 unit tests at sprint end (up
from 371).

Deliberately deferred → since landed: ruff-parser fork (vendored),
auto-gather inference + parallelisation, source-map line accuracy,
`class!` raw-class modifier with synthesised `__init__`, runtime
`stubtest` probe, comptime `def` functions, variance in `assignable`.

Deliberately deferred → still future work: `ty` integration Phase 2
(embedded library sharing the Salsa DB); higher-kinded /
bounded-typevar inference; vendoring `ruff_python_codegen` (probed and
rejected as net-negative).

### May 18 round 1 (`test-typhon-library-EnZmE` → fixed across `update-findings-IdfrH`, `fix-findings-diagnostics-hYj8K`)

Filed #1–#36, all closed except #18 (`tyc fmt`, still open as O12).
Major closures: `class Foo frozen:`, `guard NAME = EXPR else: …` (single
and multi-line), `impl[T] Box[T]:`, `gather(strategy="best-effort")`
scope, `T?` Union/Union assignability, `dict.get(k)` as `V?`,
`if x:` truthy narrowing, lazy import diagnostics, interface
conformance via `impl`-block methods, `tyc check` parity with build for
purity/comptime, missing param/return annotation enforcement, float
literal `1.0` emission, `from __future__ import annotations` injection,
comptime container literals + string methods, `tyc::result_error_mismatch`,
`tyc::method_in_class_body` with strictness toggle,
`tyc::auto_gather_missed` advice, REPL auto-print.

### May 18 round 2 (`test-typhon-library-kxGRX` → fixed in `resolve-open-findings-d6EIV`)

Filed #37–#56, all closed. Major closures: `with`-chain / best-effort
`gather:` / `go` desugarers emit `let` (Rule-2 regression cluster);
`for k, v in d.items():` tuple-target declaration; f-string format
specs / `!r`/`!s`/`!a` conversions; f-string nested same-quoted strings;
`Callable[[P], R]` callability; kwargs / defaults / `*args` arity
counting; generic constructor type-param inference (bidirectional +
forward); PEP 695 lowering for older targets; comptime cross-binding
scope + `comptime def` dispatch; `tyc::missing_await`; `tyc::manual_init`;
`tyc::generator_return_type` (warn direction); multi-line pipe; REPL
None-call skip; `extend BUILTIN:` self annotation; `tyc init NAME`
scaffolds into `./NAME/`; examples-suite pass rate 20/47 → 39/47.

### May 18 examples sweep (`review-examples-x5kgr`)

Closed 7 compiler bugs surfaced by walking every `examples/` project:
resolver `Stmt::Match` walk; resolver parameter-default walk; resolver
nested function / class declaration; `with`-chain `?` validator false
positive; format `apply_simple_style_rules` whitespace stripping;
f-string literal brace re-escaping; comptime `def` emission. Examples
suite ended at 39/47 build + 39/47 stdlib-runnable.

Filed F1–F5 cross-cutting findings: F1 (loop-target rebind, **closed
in the May 21 sweep**), F2 / F3 / F4 (missing-return analysis through
`unsafe:`, `with`, and exhaustive `match` — the May 19 / May 20
sprints chained the missing-return checker into the relevant
analyses), F5 (docs nit about declaring deps to make `tyc check` happy
in a fresh `tyc init` project — added to `examples/README.md`).

### May 19 round (`test-typhon-library-rNIYC` → fixed in `review-findings-fixes-VRFJy`, `add-library-autocomplete-1N2iS`, `resolve-open-findings-UDZfv`)

Filed #57–#127. Closures span: transparent type aliases (#57/#58/#70),
method-level typevars on `impl[T]` (#59), dependent `gather:` bindings
fall back to sequential (#60), `global` / `nonlocal` + Rule 2 (#61),
mutable dataclass defaults (#62), `@property` typing (#63),
`tyc migrate` correctness (#64), partial `tyc fmt` (#65 — still mostly
open as O12), `?` diagnostic crisp (#66 — limitation **still open as
O17**), bare `list` / `dict` / `tuple` annotations rejected (#72),
`from typing import TypeVar` rejected (#73), `typing.List` / `Dict` /
... rewritten (#74), for-loop target binding kind (#75), block-level
shadowing diagnostic (#76), class redeclaration (#77),
`impl UnknownClass:` (#78), unknown module at check time (#79),
unknown-kwarg "did you mean" (#80), circular alias (#81), missing-return
exhaustive-match chain (#82), `async def` no `await` warning (#83),
`lazy let` runtime emission (#84), auto-gather decorator gate (#85),
`*args: T, sep: str = "-"` (#86), comptime help text (#87), comptime
subscript (#88), decorator-factory docs path (#89), `self` outside
`impl` (#90), `let x: T` no init (#91), `main()` not called advice
(#92), inline comment in `gather:` (#93). Typing-bridge transparency
across `Self` / `Literal` / `Final` / `ClassVar` / `Annotated`
(#97–#100). `class X(NamedTuple):` and multi-inheritance no longer
emit `slots=True` (#101 / #102). `model X:` auto-injects pydantic
(#103). `type Err = …` no longer shadows `?` runtime constructor (#104).
`lazy let` print materialises (#105). `f"{x=}"` debug repr (#106).
`ExceptionGroup` / `BaseExceptionGroup` in builtin scope (#113).
async-protocol dunder carve-out (#114). `tyc migrate` typing-alias
rewriter (#115). `bytes` literal preservation (#116). `tyc init` entry
block (#123). Walrus carve-out documented (#124). `tyc fmt` still open
as O12. Doc drift on `lazy_val` → `lazy_let` (#126 / #127).

Still open after this round:
- #107 — `unsafe:` value leak → **O14**.
- #108 — TypedDict dict-literal inference → **O15**.
- #111 — sequence pattern bracket round-trip → **O27**.
- #112 — Protocol vs built-ins → **O16**.
- #117 / #118 / #119 — pipe corner cases → **O28**.
- #122 — `tyc fmt` → **O12**.

### May 20 round (`test-typhon-library-ejNr5` → fixed in `resolve-open-findings-v6t65`)

Filed R3.1–R3.18, all closed except `tyc fmt` (R3.15) and `?` in
sub-expression (R3.13). Major closures: walrus parens preserved (R3.1);
class-attr-shadows-slot warning (R3.2); inline `with`-chain (R3.3);
aliased `lazy import` runtime registry (R3.4); `lazy let` inside `impl`
→ `@cached_property` (R3.5); `tyc::operator_type_mismatch` for
clearly-wrong operand pairs (R3.6); kwarg validation in constructors
(R3.7); match-class pattern field validation (R3.8); `int / int` typed
as `float` (R3.9); `tyc::tuple_index_out_of_range` (R3.10); pipe-into-
method arity counts `self` (R3.11); five match exhaustiveness shapes
(R3.12); `async_without_await` (R3.14 / #83 redux); `@staticmethod` /
`@classmethod` in `impl` arity (R3.16); `stubs/` directory doc fix
(R3.17); `.pyi` stub decorator stripping (R3.18).

### May 20 exploration (`typhon-exploration-testing-LZezp`)

Filed E1–E11. Verified-fixed at filing time: R3.1, R3.6, R3.7, R3.8,
R3.9, R3.10, R3.11, R3.14, R3.16. The May 21 findings sweep then
closed E1 (O1, paren-emit), E2 (O7, impl `T?`), E3 (O8, `return self`),
E4 (O9, `Callable` alias), E5 (O18, `__mul__`), E7 (O20, comptime
f-string), E10 (O11, for-rebind). Still open: E6 → **O19**, E9 →
**O17**, E11 → **O29**. (E8 was a re-statement of R3.2, closed in
`resolve-open-findings-v6t65`.)

### May 21 round (`tender-hawking-LLhuR`)

Filed B1–B14 (81 fresh `.ty` programs, 65/81 build + run clean; 7 real
bugs surfaced). The May 21 findings sweep closed B1 (O3), B2 (O4),
B3 (O6), B4 (O5), B5 (O2), B6 (O21), B7 (O22), B8 (O23), B10 (O10),
B11 (O24), B12 (O25), B13 (O26), and B14 (O13 — already-fixed). Only
B9 (`tyc fmt`, **O12**) is still open from this round.

Repro corpus + run script at `stress/round-2026-05-21/`. The
intentionally-broken probes under `10-error-quality/` exist to validate
diagnostic message quality and are expected to fail at build time.

### May 21 findings sweep (`claude/findings-documentation-review-HhuVH`)

No new findings filed; this round walked every Open finding in the
table above and either closed it with a code fix or verified it was
already closed. Eight bugs closed by code; seven verified-fixed (the
findings doc was stale on those). Compiler-wide tests at 1275+, all
green.

### May 21 follow-up sweep (`claude/finish-open-findings-dmLv6`)

Closed every Open finding remaining after the May 21 sweep. Seven new
diagnostic / desugar / type-check landings:

1. **O12** — `tyc fmt` now applies five PEP 8 rules end-to-end.
2. **O17** — inline `?` operator (`Ok(f(x)?)`) lifts to a temp.
3. **O27** — sequence-pattern bracket style round-trips via
   source-range peek.
4. **O29** — `--no-sync` / `TYC_NO_SYNC=1` skips `uv sync`.
5. **O15** — TypedDict dict-literal inference against class shapes.
6. **O16** — built-in containers satisfy Protocol structural checks.
7. **O14** — new `tyc::unsafe_value_leak` diagnostic enforces Rule 5.
8. **O28** — pipe corner cases (parenthesised callable, pipes in
   sub-expressions).

O19 was verified-fixed against its three known repros — every shape
type-checks cleanly today even though the find filed in E6 was still
listed as Open. Compiler-wide tests at 1306+, all green. Shipped as
[v0.3.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.3.0).

### May 22 round (`claude/typhon-library-testing-ch1yz`) — v0.3.1

A second 0.3-series stress campaign against the v0.3.0 release —
93 fresh `.ty` programs across nine domain buckets (`01-language-edge`
through `09-error-quality`). Repro corpus at
`stress/round-2026-05-22/`. Filed N1–N13: three CRITICAL (silent
wrong output on `not (X or Y)` / `not (X if C else Y)` emit, VM
`match`-arm scope loss), five HIGH (multi-line `freeze let`, `?` in
comprehension, `tyc run` skipping the static check, `tyc migrate`
output for `Generic[T]` not building, `match self.<field>:` false-
positive `missing_return`), two MEDIUM (no typed tuple-unpacking
`let`, silent `impl`/`extend` method merge), three LOW (comptime
`str.join`, wrong-direction newtype diagnostic, duplicated future-
import). All 13 closed before the v0.3.1 tag; see
[Recently closed: 2026-05-22 stress round](#recently-closed-2026-05-22-stress-round-n1n13)
for the per-finding write-up.
