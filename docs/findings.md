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

**Eight stress-test campaigns** have been run between May 17 and May 21,
2026. Across them ~600 hand-written `.ty` programs were authored
spanning language edge cases, IO, ML/numpy, mock LLM/RAG, agents, APIs,
SDK patterns, perf, and intentionally-broken diagnostic probes. Roughly
**120 distinct findings** were filed.

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
| 2026-05-21 | `claude/tender-hawking-LLhuR` | B1–B14 | closed B1–B7, B10, B12, B13; B8, B9, B11, B14 still open |
| 2026-05-21 findings sweep | `claude/findings-documentation-review-HhuVH` | — | closed O2/O3/O4/O5/O6/O10/O21/O22/O25/O26; verified-fixed O1/O7/O8/O9/O11/O18/O20 |

**Pass rate trend** on the canonical example suite (`examples/01-…46-…`):
20/47 → 39/47 → 46/46 → 46/46. The examples now build and run end-to-end
on every commit; new findings come from the stress corpora rather than
the curated examples.

---

## Open findings

Severity legend: **CRITICAL** (silent wrong output / runtime crash on
documented happy path), **HIGH** (documented feature unusable),
**MEDIUM** (feature works with workaround), **LOW** (UX / DX nit /
docs).

### MEDIUM — workable but rough

#### O12 — `tyc fmt` is a near-no-op *(B9, R3.15, #18, #65, #122)*

Repro: feed any `.ty` with cramped spacing through `tyc fmt`. Tracked
across every campaign since the very first.

```python
def    f(  x:int,y:int)->int:
    let    z:int=x+y
```

After `tyc fmt`:

```python
def f(x:int,y:int)->int:    # missing spaces around : , -> = preserved
    let z:int=x+y
```

The Phase-5 Typhon-aware printer documented at
`tyc-format/src/lib.rs:17` remains future work. Ruff is on PATH but
ruff doesn't speak `let`/`mut`, so the fmt wrapper can only do a
partial whitespace pass.

`tyc build` *does* run `ruff format` over the emitted Python (correctly,
end-to-end). The deliverable is clean; the source isn't.

#### O13 — Pydantic dep not auto-injected when project lacks `[dependencies]` *(B14)*

The fix from `#103` adds `pydantic = "*"` to a project's synthesised
`pyproject.toml` when any source file contains `model X:`. However when
a project's `typhon.toml` has no `[dependencies]` section at all, the
auto-inject can fail to surface (depends on the bootstrapping path
taken). Reproduced in the round-2026-05-21 stress harness, which
synthesises a fresh `typhon.toml` per case. Either tighten the detection
path or document the requirement to declare at least an empty
`[dependencies]` table for the auto-inject to fire.

#### O14 — `unsafe:` value leak isn't caught *(#107)*

A binding declared inside `unsafe:` can be returned from a function
whose annotated return type is concrete (e.g. `-> int`) without
re-asserting, contradicting Rule 5 in the language spec. Needs a real
`Unsafe[T]` marker type in `tyc-types` and a flow-sensitive pass through
the block boundary.

#### O15 — TypedDict dict-literal inference *(#108)*

```python
let alice: User = {"id": 1, "name": "Alice"}
# tyc::type_mismatch: expected User, found dict[str, int | str]
```

The `User(id=…, name=…)` constructor form works; the dict-literal form
doesn't. Needs the type-checker to register TypedDict-derived class
shapes and match dict literals against the expected field set.

#### O16 — `Protocol`-based structural conformance against built-ins *(#112)*

```python
class Sized(Protocol):
    def __len__(self) -> int: ...

def take(s: Sized) -> int: return len(s)

take([1])      # tyc::type_mismatch: expected Sized, found list[int]
```

Built-ins don't participate in structural conformance against
user-declared `Protocol`s. Needs the type-checker to know which
built-in types implement which dunder methods (`__len__`, `__iter__`,
etc.).

#### O17 — `?` operator only works at the top of an assignment / statement *(#66, R3.13, E9)*

```python
def f(s: str, t: str) -> Result[int, str]:
    return Ok(add(parse(s)?, parse(t)?))   # tyc::invalid_question_op
```

The diagnostic is now crisp ("lift the inner call to a `let` binding
first") and points at the user's source — that landed in the May 19
sprint. Lifting the limitation itself (rewriting `Ok(f(x)?)` into a
temp + propagation) remains future work and is the natural Rust-style
form users reach for.

#### O19 — Exhaustiveness false negatives on sealed unions with positional captures *(E6, partial)*

Several legitimate-total matches still mis-fire as
`tyc::missing_return`. R3.12 closed five of the most common shapes;
recursive sealed types with positional captures, multi-variant
dataclass enums with empty variants, and three-variant tool unions
with positional captures still surface as false positives. Repros at
`01-syntax-edge/05-sealed-recursive.ty`,
`05-agents/03-agent-state-machine.ty`,
`04-ai-llm/03-llm-tools-sealed.ty`.

Workaround `case _:` works but defeats the safety property.

### LOW — DX / cosmetic

#### O23 — `extend list[int]:` (parametric) mis-targets `list` *(B8)*

Repro: `stress/round-2026-05-21/01-language-edge/16-extend-builtin.ty`
(initial form).

```python
extend list[int]:
    def total(self) -> int: ...
```

Diagnostic claims the user wrote `impl list:`. Either accept
parametric extends or surface a
`extend on parameterised types is not supported yet` error.

#### O24 — VM repr disagrees with CPython repr for `Result` *(B11)*

VM prints `Ok(20)`; CPython prints `Ok(value=20)`. Causes `tyc run`
and `tyc run --compile` to diverge in stdout, which the `tyc-vm` doc
explicitly warns about, but worth surfacing for screenshot-driven
docs and test fixtures.

#### O27 — Sequence pattern `[a, b]` emits as tuple pattern `(a, b)` *(#111)*

PEP 634 makes these semantically identical (both match any sequence)
and the runtime behaviour is correct. Round-tripping the original
bracket choice requires source-range tracking on the pattern node
and is purely cosmetic.

#### O28 — Pipe corner cases *(#117, #118, #119)*

- `5 |> (lambda x: x * 2)()` is a parse error — pipe RHS doesn't
  accept a parenthesised lambda call.
- `s |> S.add(5)` desugars to `S.add(s, 5)` and the call-site arity
  check rejects it because `impl`-block method types don't include
  `self` in the function arity the checker sees. (R3.11 closed the
  basic pipe-into-impl case; the variant where `self` is the
  receiver via class-name dispatch is still open.)
- `(1 |> add(2)) |> add(3)` — pipe `|>` is not recognised inside a
  parenthesised sub-expression: parser fails at the inner `>`. Pipe
  is restricted to top-of-statement positions.

Pipe expansion is a top-of-statement-line pre-pass; lifting it into
general expression position needs a lexer-aware rewrite.

#### O29 — Per-invocation `uv sync` provisioning *(E11)*

Every `tyc build` invocation against a project with non-empty
`[dependencies]` spawns `uv sync` and may reprovision `.venv` from
scratch. For one-off `.ty` files under tmp directories (stress
harnesses, REPL-like testing), this dominates wall-clock time.
A `--no-deps` / `--reuse-venv` flag would speed iteration.

---

## Recommended next steps

Roughly ranked by impact per unit of work, restricted to the open
items:

1. **O15 (TypedDict dict-literal inference)** — `let alice: User =
   {"id": 1, "name": "Alice"}` is the canonical Python idiom; users
   reach for it first and fall back to the kwarg constructor only
   after the diagnostic fires. Needs the type-checker to register
   TypedDict shapes and match dict literals slot-by-slot.
2. **O16 (Protocol vs built-ins)** — once built-ins carry their
   dunder shape in the registry, every structural-typed library
   call site benefits (`len`, `iter`, `enter`, `exit`).
3. **O12 (`tyc fmt`)** — pick three rules (no-space-before-colon,
   single-space-around-`=`/`+`, two-blank-lines before top-level
   defs). Anything is better than today.
4. **O17 (`?` in sub-expression)** — lifting `Ok(f(x)?)` is the
   natural Rust-style form. The desugar pass already has the
   temp-lifting machinery for the top-of-statement case.
5. **O14 (`unsafe:` value leak)** — Rule 5 in the language spec is
   not enforced today. Needs an `Unsafe[T]` marker in `tyc-types`
   and a flow-sensitive boundary check.
6. **O19 (sealed-union exhaustiveness)** — three known repro
   patterns; partial-fix machinery from R3.12 already in place.
7. **O28 (pipe corner cases)** — lifting `|>` into expression
   position is a lexer-aware rewrite; tractable but invasive.
8. **O13 (Pydantic auto-inject)** — tighten the detection path so
   it fires even on bare `typhon.toml` projects.
9. **O23 (parametric `extend`)** — at minimum a clearer
   "parameterised extends not yet supported" diagnostic than the
   current `impl_unknown_class` cascade.

The remaining open items are smaller papercuts.

---

## Recently closed (this branch)

Branch: `claude/findings-documentation-review-HhuVH`.

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
B3 (O6), B4 (O5), B5 (O2), B6 (O21), B7 (O22), B10 (O10), B12 (O25),
B13 (O26). Still open:

- B8 → **O23** (parametric `extend`)
- B9 → **O12** (`tyc fmt`)
- B11 → **O24** (VM repr)
- B14 → **O13** (Pydantic dep auto-inject)

Repro corpus + run script at `stress/round-2026-05-21/`. The
intentionally-broken probes under `10-error-quality/` exist to validate
diagnostic message quality and are expected to fail at build time.

### May 21 findings sweep (`claude/findings-documentation-review-HhuVH`) — this branch

No new findings filed; this round walked every Open finding in the
table above and either closed it with a code fix or verified it was
already closed. See the "Recently closed" section above for the full
list. Eight bugs closed by code; seven verified-fixed (the findings
doc was stale on those). Compiler-wide tests at 1275+, all green.
