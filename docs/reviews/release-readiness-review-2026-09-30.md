# Typhon — 1.0 release-readiness review (2026-09-30)

**Reviewed commit:** `0c439a3` (main; the alpha.9 line plus 51 unreleased
commits — 512 files, +54,689 / −5,368 lines — that the docs already label
`v1.0.0-beta.1`). **Binary under test:** release `tyc 1.0.0-alpha.9` built
from that commit with Rust 1.94.1; CPython 3.13.12; ruff 0.15.8.
**Method:** the five CI gates run locally, the full example + stress corpus
type-checked and executed both ways, an LSP handshake, the new-user flow
(`init` → `check` → `run` → `build`), `migrate` / `fmt` / `repl` smoke tests,
and about 200 hand-written probe programs, each run through `tyc check`,
the VM (`tyc run --no-fallback`) and CPython (`tyc run --compile --temp`),
with the two outputs diffed. Every finding below was reproduced against the
binary; the reproductions are short enough to be quoted inline.

## Verdict

**Not ready for a 1.0 "full release". Ready for the beta the tree already
names, once one blocker is fixed.**

The engineering practice is unusually good for a project this size: every
gate is real and loud, the docs match the binary to within a few lines, the
release pipeline is pinned, checksummed and least-privilege, and the
VM ↔ CPython differential over 1,480 units is down to seven inherent
divergences. Since the 2026-09-01 review the two structural risks it named
(the text-rewriting preprocessor and the VM's stdlib tail) have been dealt
with convincingly: **every accepted preprocessor and VM probe in this review
produced byte-identical output on both surfaces.**

What keeps it short of 1.0 is the type checker's headline promises. Rule 3
("`T` cannot hold `None`") and the sealed-union guarantee still have holes
that ordinary code walks into — a `dict.get(k).method()` call, a
`with`-chain binding, a field read on a union — and each of them is a
`tyc check` exit 0 followed by a `TypeError` / `AttributeError` at runtime.
There is also a hard blocker on the release path itself: the `security` CI
job fails on the current lockfile.

| Question | Answer |
|---|---|
| Can it ship a `v1.0.0-beta.1`? | Yes, after the salsa advisory is fixed (a small, contained bump). |
| Can it ship `v1.0.0`? | Not yet. Close the soundness holes in §3, fix the package-layout false positive in §4, then freeze the syntax and let it sit in beta with real users. |

## 1. Gate results at the reviewed commit

| Gate | Result |
|---|---|
| `cargo fmt -- --check` | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass |
| `cargo test --workspace` (`TYC_REQUIRE_PYTHON=1`, `TYC_REQUIRE_RUFF=1`) | **3,027 passed, 0 failed, 3 ignored** |
| `cargo deny check` | **FAIL** — `RUSTSEC-2026-0308` (see §2) |
| Perf gate (`examples/47-mini-app`) | flaps at the margin: median 27–28 ms against a 22 ms baseline whose limit is 27 ms; 1 fail / 3 pass in four consecutive runs on a quiet machine |
| VM ↔ CPython differential (1,480 units, `--jobs 2`) | **PASS** — 1,075 agree, 7 diverge (all pinned), 92 vacuous, 42 vacuous-runtime, 0 non-compiling, 253 nobuild, 11 nondeterministic |
| Opt-in knob matrix | 12 / 12 pass |
| `tyc check` over `examples/` (49 units) | 49 clean (7 carry only the unintrospectable-dependency warning, expected without a venv) |
| `tyc check` over `stress/` (1,431 units) | 252 fail — every one listed in `nobuild-baseline.txt`, whose one remaining entry is a sourceless project that fails only at build; nothing else |
| LSP (`initialize` / `didOpen` / `hover` / `shutdown`) | works; a diagnostic below a `?` expansion reports the correct `.ty` line |
| New-user flow, `migrate`, `fmt` fixed point, `repl`, bundled `httpx` stub | all work (details in §7) |

Two notes on the gates themselves:

- **CI on `main` is green only because nothing has run since 2026-09-02.**
  The salsa advisory was published on 2026-09-24. The next push to `main`
  turns the `security` job red, and because `auto-tag.yml` gates the release
  workflow on a green CI run, no release can be cut until it is fixed.
- **The perf gate's baseline is machine-specific.** A 22 ms median with a
  5 ms floor is inside process-start jitter on a different host. It will
  produce red runs on hardware other than the recording machine; either widen
  the floor or make the gate relative to a same-run control build.

## 2. Blocker: `RUSTSEC-2026-0308` in salsa 0.26.2

`cargo deny check advisories` fails on the pinned `salsa = "0.26"`
(resolved 0.26.2). The advisory is classed **unsound** — a use-after-free
reachable through safe APIs when interned values are reused across
revisions — and the affected crate is the incremental database that the
language server runs over attacker-controlled input (any `.ty` file opened
in an editor). Fixed in salsa ≥ 0.28.5.

The bump is contained. Salsa is used only from `tyc-db/src/lib.rs`
(42 references) and one bench, exactly as the project's "wrap every external
crate" rule intends. A trial `salsa = "0.28"` produced four compile errors,
all `cannot find trait Update in crate salsa` in `tyc-db`, and nothing
elsewhere in the workspace. Expect an hour's work plus a benchmark re-run.

While in there: `cargo update --dry-run` lists 74 semver-compatible crate
updates pending; the lockfile has not moved since the alpha.9 dependency
wave.

## 3. Type checker — unsound accepts

Every entry here is a program `tyc check` accepts with no error and that
raises on both CPython and the VM. Under the project's own compatibility
taxonomy each fix is class **C** (a narrowing on code that already crashes),
so none of them threatens the "additive on correct programs" contract.
"New" means the 2026-09-01 review's open list does not contain it.

### 3.1 A nullable call or subscript result used as a receiver — *new, high*

Rule 3 is enforced for a bare name and for an argument position, but not for
a receiver that is itself a call or a subscript. None of these reports
anything:

```typhon
let d: dict[str, str] = {"a": "x"}
print(d.get("b").upper())          # AttributeError: 'NoneType' has no attribute 'upper'
print(find(1).upper())             # find -> str?
print(xs[0].upper())               # xs: list[str?]
print(find_c(1).n)                 # find_c -> C?      (attribute)
print(find_l(1)[0])                # find_l -> list[int]?  (subscript)
print(f"{find(1).upper()}")        # inside an f-string
print(R(v=None).get().upper())     # method returning str?
```

The bound form is caught (`let v: str? = d.get("b"); v.upper()` →
`tyc::nullable_use`), and so is `shout(d.get("b"))` (→ `type_mismatch`).
The gap is specifically the receiver position of `.attr`, `.method()` and
`[i]` when the receiver expression is not a name. This is the single most
common shape of the bug the language exists to prevent.

### 3.2 `with`-chain bindings are untyped — *new, high*

The bindings introduced by a Result `with`-chain carry no type:

```typhon
def parse(s: str) -> Result[int?, str]:
    return Ok(None)

def f() -> Result[int, str]:
    with c = parse("3")?:
        return Ok(c + 1)            # accepted; TypeError at runtime
```

and

```typhon
with c = parse("3")?, d = parse("4")?:
    let bad: str = c + d            # accepted (c + d is int)
```

The chain *body* is otherwise checked (`let bad: str = 1` in it is rejected,
and so is the `else err:` body), so it is the binding types that are lost,
not the suite.

### 3.3 Member access on a sealed-union value without narrowing — *new, high*

```typhon
class A:
    n: int
class B:
    s: str
type U = A | B

def f(u: U) -> int:
    return u.n                      # accepted; AttributeError when u is a B
```

Also `u.go()` where only `A` has `go`. A field common to every variant is
correctly accepted, and the builtin case (`v.upper()` on `int | str`) is
correctly rejected, so the check exists but does not run on user-class
unions — which is the only kind the sealed-union feature is for.

### 3.4 A closure's `nonlocal` write does not invalidate the enclosing narrowing — *new, medium*

```typhon
def main() -> None:
    mut x: int? = 1
    def clear() -> None:
        nonlocal x
        x = None
    if x is not None:
        clear()
        print(x + 1)                # accepted; TypeError
```

Same result when `clear` is defined inside the `if`. The alpha.2 rule
("a call invalidates a non-local narrowing") covers globals and fields;
a local captured by `nonlocal` is the missing case.

### 3.5 Wrong arity through a `Callable`-typed value — *new, medium*

```typhon
let f: Callable[[int, int], int] = lambda a, b: a + b
print(f(1))                         # accepted; TypeError
```

Also through a field (`h.fn(1)`) and a parameter. Argument *types* through
a `Callable` are checked (`f("x")` is rejected); only the count is not. A
plain function called with the wrong arity is rejected as expected.

### 3.6 `await` on a non-awaitable — *new, medium, plus a VM divergence*

```typhon
async def main() -> None:
    let n: int = 1
    let v: int = await n            # accepted
```

Same for `await "x"` and `await sync_fn()`. Under CPython this is a
`TypeError`; **the VM runs it and returns the value** (exit 0), so it is
also a VM ↔ CPython divergence the differential corpus does not contain.

> **Note (2026-10-03):** for the false-positive side of this area — valid
> `await` forms the checker wrongly rejects — see the Kimi findings §7,
> which catalogues the awaitable shapes worth covering when this item is
> worked.

### 3.7 Comparison operators are not type-checked — *new, medium*

`"a" < 1`, `1 <= "a"`, `P(x=1) < P(x=2)` on a class with no `__lt__`, and
`3 in 5` are all accepted and all raise. Arithmetic is checked
(`"a" + 1` → `tyc::operator_type_mismatch`); ordering comparisons and `in`
are not.

### 3.8 Still open from the 2026-09-01 review (re-confirmed)

| Finding | Probe result |
|---|---|
| Attribute narrowing survives a write through an alias (`other.val = None` after narrowing `b.val`) or a free call receiving the object | accepted, `TypeError` |
| A subclass redeclares a field with an incompatible type (`class Sub(Base): x: str`) | accepted, `TypeError` in the base-typed consumer |
| `x[0]` on `int \| str` | accepted, `TypeError` |
| `2 ** e` with a negative `e` typed `int` | accepted, `AttributeError` on `.bit_length()` |
| Positional-only parameter passed by keyword | accepted, `TypeError` |
| `for x in obj` over a class with no `__iter__` | accepted, `TypeError` |
| Writing an undeclared attribute on a `@dataclass(slots=True)` instance | accepted, `AttributeError` |
| `go f()` inside a sync `def` | accepted; CPython raises `no running event loop`, **the VM prints nothing and exits 0** |
| Typed tuple unpack with the wrong arity (`let (a: int, b: int) = three_tuple`) | accepted; the lowering indexes `[0]`, `[1]` so it *silently drops* the third element where Python would raise `ValueError` |

### 3.9 Warn-level defaults that let a crash through

These are documented and configurable, but each is the headline promise
firing as a warning:

- `self.conn.query()` with `conn: Conn?` (and `r.conn.q()` on a non-`self`
  receiver) is `tyc::nullable_use` at **warn** under the default
  `[strictness] nullable-use = "warn"`. It crashes.
- `tyc::incompatible_override` is a warning; the narrowed override then fails
  at runtime through the base-typed call.
- `del x; print(x)` is `tyc::possibly_unbound` — a warning for a *provable*
  `UnboundLocalError`.

For a 1.0 the first of these should default to `error`; the other two are a
judgement call.

## 4. Type checker — false positives

Idiomatic programs the checker rejects. The first two matter most because
they block layouts real projects use.

### 4.1 Relative imports inside a package break sealed-union exhaustiveness — *high*

```typhon
# src/shapes/ops.ty
from .kinds import Shape, Circle, Rect      # Shape is `pub type Shape = Circle | Rect`

pub def area(s: Shape) -> float:
    match s:
        case Circle(r):
            return 3.0 * r * r
        case Rect(w, h):
            return w * h                    # tyc::missing_return
```

Changing the import to `from shapes.kinds import …` makes it pass. The
relative form is what `tyc migrate` produces and what every Python
package uses. Note that the *negative* direction works through the same
path — a missing `Rect` arm is reported through a `pub *` facade — so the
registry is reachable; it is the relative-import resolution that loses the
alias.

### 4.2 A `newtype` imported through a `pub *` facade does not widen — *medium*

```typhon
from shapes import ShapeId          # re-exported by `pub *` in shapes/__init__.ty
let raw: int = ShapeId(3)           # type mismatch: expected `int`, found `ShapeId`
```

`from shapes.kinds import ShapeId` widens correctly.

### 4.3 `missing_return` on `try`/`else` and `for`/`else` — *medium*

```typhon
def safe_div(a: int, b: int) -> float:
    try:
        let r: float = a / b
    except ZeroDivisionError:
        return 0.0
    else:
        return r                    # "missing a return on some paths"
```

and the same for `for … else: return -1` with a `return` inside the loop.

### 4.4 Smaller

- `let (first: int, *rest) = t` is a parse error ("Expected `)`, found `:`");
  the untyped `let (first, *rest) = t` works. The message should say that a
  starred target cannot be annotated.
- `err` inside a multi-line f-string in an `else err:` block →
  `tyc::unknown_name` (still open from the previous review).
- PEP 696 defaults: `class Box[T = int]` then `let c: Box = Box(v=1)` →
  "expected `Box`, found `Box[int]`" (still open).
- A `class!` subclass (no `@dataclass`, no slots) gets
  `tyc::class_attr_shadows_slot`, whose message says its attribute "will be
  a slot descriptor"; the same warning fires on a subclass whose only
  *own* field is defaulted while it inherits a required one.
- `lazy let cfg: int = compute()` inside a `class!` body works (it is lazy)
  but is reported as `tyc::method_in_class_body` ("method `cfg` defined
  inside class body").
- A postfix `rescue` in a `-> None` function reports "type mismatch:
  expected `None`, found `Err[int]`" rather than naming `rescue` / `?`.

### 4.5 Documented variance inference does not reach idiomatic code — *docs vs compiler*

`TYPE_SYSTEM_FRONTIER.md` and the README advertise user-generic variance
inference with `@covariant` / `@contravariant` overrides. In practice:

- `@covariant` on a class is `tyc::unknown_name` — the decorator is not
  resolvable, so the override cannot be written.
- Inference only fires for methods written *inside the class body*, the
  shape `tyc::method_in_class_body` warns against. A `class Producer[T]
  frozen:` with `item: T` and an `impl[T] Producer[T]: def get(self) -> T`
  infers **invariant**; so does a class whose only use of `T` is an
  `impl`-block return type.

The unit tests cover the class-body shape and pass, which is why this has
not shown up. Either wire `impl` blocks and frozen fields into the
classification and register the decorator, or drop the claim from the docs
before 1.0.

### 4.6 Diagnostic completeness across modules

An imported `frozen` class's field write (`c.r = 5.0` on a facade- or
directly-imported `Circle`) is reported **only when the module has no other
error**. With any unrelated error in the same file — a `type_mismatch`, a
`missing_return` — the `frozen_assign` disappears. Single-file programs are
unaffected (a local frozen class reports alongside other errors), so it is
the cross-module shape pass that is being skipped on the first error. A user
fixes one error and a new one appears.

## 5. Preprocessor, desugar and emitter

This is where the previous review found its worst class of bug, and it is
the area that has improved most. Every one of the following produced
byte-identical stdout on the VM and CPython, with the expected exit codes:

`?` on a one-line compound statement (`if flag: x = f()?` stays
conditional; `while f()?: … else:` runs its `else`), `?` and `|>` and `as!`
and `rescue` inside string / bytes / f-string / triple-quoted literals, `|>`
with keyword arguments and across parenthesised lines with comments,
`as!` in `for` iterables / conditions / comprehensions / nested calls,
`rescue` in loops with `continue`, `gather:` inside `try` with `except*`,
`gather(strategy="best-effort")`, one-line `enum` / `gather:` bodies,
column-0 comments inside `enum` and `gather:` bodies, tab and two-space
indentation with `?` in `elif` / `while`, CRLF, a UTF-8 BOM, no trailing
newline, non-ASCII identifiers, PEP 701 same-quote f-string fields with
`?` and `|>` in the keys, `freeze let` over a triple-quoted string,
nested `with`-chains, multi-line `go … -> t` with comments, `class!`
grandchildren, mutable and dataclass-instance field defaults, `ClassVar`
registries, `enum.Flag`, `lazy let` deferring to first use, `comptime`
constants, `super()` under slots, PEP 3134 exception chaining, `except*`,
generator `send`, and `pub` → `__all__`.

One miscompile class remains open from the previous review:

- **`extend BUILTIN` on an attribute or call receiver.** `self.title.slug()`
  and `make().slug()` pass `tyc check` (the extension is resolved) but are
  never rewritten to the free-function call, so both surfaces raise
  `AttributeError: 'str' object has no attribute 'slug'`. A bare
  `title.slug()` works. This is exactly the "exit 0, wrong program" shape.

The `-O` profile's purity verifier is now honest: under `tyc build -O` only
`pure_add` was cached; functions calling `datetime.now()`, `random.randint`,
`time.time()` or reading a module `mut` were left alone, and a parameter
shadowing a `@gatherable` name was not folded.

## 6. VM ↔ CPython

The drop-in contract is now *decided and enforced*: an unmodelled import
takes the compiled path automatically. Of 64 common stdlib modules probed,
30 are modelled and 34 fall back (`copy`, `textwrap`, `statistics`,
`logging`, `uuid`, `decimal`, `subprocess`, …), which is fine by design.

What the import scan cannot catch is a missing *attribute* of a modelled
module, and that is where the remaining divergences live. Each of these runs
under CPython and fails under `tyc run`:

| Divergence | Detail |
|---|---|
| `import pkg.sub` then `pkg.sub.attr` | `AttributeError: module 'pkg' has no attribute 'sub'`. `import pkg.sub as s`, `from pkg import sub` and `from pkg.sub import x` all work — only the plain dotted `import` fails to bind the submodule on the parent |
| Multi-iterable `map(f, xs, ys)` | `TypeError: <lambda>() missing required argument: 'b'` |
| `math.isclose` | missing; `math` lacks 20 of 62 names (hyperbolics, `gamma`, `frexp`, `ldexp`, `modf`, …) |
| `re.I` / `re.M` / `re.S` / `re.X` short flags, `re.Pattern`, `re.Match`, `re.error` | missing (the long names exist) |
| `functools` (10 of 19 missing: `update_wrapper`, `partialmethod`, `singledispatchmethod`, …), `contextlib` (14 of 22: `AbstractContextManager`, `AsyncExitStack`, `chdir`, `aclosing`, …), `heapq.merge` / `heappushpop` / `heapreplace`, `hashlib` SHA-3 / `pbkdf2_hmac`, `operator` in-place ops, `collections.UserDict` / `UserList` | missing |
| `await 1` / `await "x"` / `await sync_call()` | the VM returns the value; CPython raises `TypeError` |
| `go f()` from a sync function | the VM prints nothing and exits 0; CPython raises `RuntimeError` |

The last two are the ones that matter for the contract, because a program
can pass `tyc check` (§3.6, §3.8) and then behave differently on the two
surfaces. The attribute gaps are less dangerous — they fail loudly — but they
undercut "drop-in" for exactly the programs that stay inside the modelled
subset. A cheap mitigation is to extend the pre-run scan from module names to
`module.attr` references against a generated table of what each shim
exports; the table in this review took one probe to produce.

## 7. Tooling, release engineering, security, docs

**Works, verified:**

- `tyc init demo` → `check` → `run` (VM) → `build` → `python3.13 build/main.py`
  produce identical output; `build/.sourcemaps/main.py.map` is written.
- `tyc fmt` reaches a fixed point in one pass; `fmt --check` exits 1 on an
  unformatted file.
- `tyc migrate` on a typed Python module (`NewType`, `Protocol`,
  `@dataclass(frozen=True)`, `Optional`, `List`) produces idiomatic Typhon
  (`newtype`, `interface`, `class … frozen`, `impl`, `T?`, `list[T]`) that
  checks clean.
- `tyc repl` evaluates and prints; the bundled `httpx` stub rejects
  `httpx.get(123)` with no venv present.
- `tyc check` is linear again: 4,000 one-`if` functions check in 0.57 s.
- The installer is HTTPS-only with TLS 1.2+, verifies SHA-256 against the
  release's `SHA256SUMS`, filters draft releases and resolves pre-releases;
  `release.yml` ships both licence texts and refuses to package without
  them; `auto-tag.yml` only fires on a green CI run from this repository.
- `SECURITY.md` states the trust boundary plainly (introspection imports
  dependencies; `TYC_NO_INTROSPECT` / `TYC_NO_SYNC` are the kill-switches;
  the introspection subprocess runs in a private scratch directory).
- All 92 `tyc::` codes have a `docs/diagnostics/` page and a `tyc explain`
  entry; the docs-site references all 92; the CLI pages document exactly
  the flags the binary accepts; version strings agree across README,
  `CLAUDE.md`, `docs/install.md`, the skill and the docs-site.

**Needs attention before a public 1.0:**

- **Repository hygiene.** 52 open pull requests, every one bot-authored
  (Jules "Sentinel" / "Palette" / "Bolt" and Dependabot), with heavy
  duplication — seven near-identical "add `OAUTH_TOKEN` to the secret
  keywords" PRs and seven "tactile `:active` state" PRs. Zero open issues.
  The bots are right about one thing: `OAUTH_TOKEN` / `OAUTH_SECRET` are not
  in `SECRET_NAME_KEYWORDS`. Merge one of each and close the rest, or turn
  the bots down; a first-time visitor reads the PR list as abandonment.
- **No evidence of external use.** Zero user-filed issues, download counts
  of 0–1 on the assets of the release GitHub marks "latest" (v0.15.7), and
  every PR from the maintainer or a bot. A 1.0 is a stability promise to users; there is no feedback loop yet
  to know what they will hit. This is the strongest argument for a beta
  period rather than a version number.
- **The syntax is not frozen.** README still says the surface "may change
  before `1.0.0`". A 1.0 needs the freeze to have happened, a stated
  compatibility policy (what "additive" means going forward; how a class-C
  narrowing is announced), and a deprecation mechanism.
- **Code shape.** `tyc-types/src/lib.rs` is 36,319 lines (1.47 MB) in one
  file; `preprocess.rs` 14,970; `interp.rs` 12,790; `builtins.rs` 11,756.
  This is a maintainability and review-ability risk more than a
  correctness one, but it is where the §3 holes live, and it is why the
  variance-inference gap (§4.5) could hide behind passing unit tests.
- **The perf gate** (see §1) will flap on any machine but the recording one.

## 8. Recommended gate for `v1.0.0`

In order of leverage:

1. **Release path:** bump salsa to ≥ 0.28.5 (`tyc-db` only), refresh the
   lockfile, get `cargo deny` green, then cut `v1.0.0-beta.1` from the
   current tree — the unreleased delta is already larger than most of the
   alpha series combined and the docs describe it as beta.1.
2. **Rule 3 completeness:** check the receiver position of `.attr`,
   `.method()` and `[i]` when the receiver is a call or subscript (§3.1);
   type `with`-chain bindings (§3.2); run the union member check on
   user-class unions (§3.3); default `nullable-use` to `error` (§3.9). All
   four are class-C narrowings.
3. **The rest of §3:** `nonlocal` invalidation, `Callable` arity,
   awaitability, comparison operators, and the re-confirmed list in §3.8.
   Each is bounded; together they are a week.
4. **Package layouts:** relative-import sealed unions (§4.1), facade
   newtypes (§4.2), and the cross-module `frozen_assign` suppression (§4.6).
   Add a multi-file project with relative imports and a `pub *` facade to
   `examples/apps/` so the corpus covers it.
5. **`extend BUILTIN` receivers** (§5): either rewrite attribute / call
   receivers or reject them at check time. Do not leave the exit-0 path.
6. **VM:** bind `pkg.sub` on plain dotted import, multi-iterable `map`,
   raise on `await` of a non-awaitable and on `go` outside a loop; generate
   the per-shim export table and use it in the pre-run scan (§6).
7. **Docs honesty:** either make `@covariant` / `impl`-aware variance
   inference real or remove the claim (§4.5); fix the two spurious
   warnings (§4.4).
8. **Process:** freeze the syntax and write the compatibility policy; clear
   the bot PR backlog; run beta.1 with at least one external project for a
   few weeks and fix what they report before tagging 1.0.

Items 2–5 are the difference between "the promise is documented" and "the
promise holds". The previous review's structural recommendations (one
lexical mask, semantics never inferred from surface text) have visibly been
followed; this review's equivalent is **one receiver-typing path** — every
`.attr` / `.method()` / `[i]` should go through the same nullable and
union check regardless of the receiver's syntactic shape.

## 9. Follow-up — remediation on the same branch

Everything above was actioned on this branch after the review was written;
this section records what changed, what the review got wrong, and what is
deliberately left for the maintainer. The changelog's *third wave* entry
carries the user-facing prose.

### 9.1 Status of each finding

| § | Finding | Outcome |
|---|---|---|
| 2 | `RUSTSEC-2026-0308` in salsa | Fixed — salsa 0.28.5; `cargo deny` green. |
| 3.1 | Nullable call / subscript receivers | Fixed — every receiver shape reports; always-`None` receivers get their own wording. |
| 3.2 | `with`-chain bindings untyped | Fixed — the chain body and `else` binding are typed; `{err}` inside a multi-line f-string is renamed (a lexical-mask bug: the f-prefix did not travel across physical lines). |
| 3.3 | Member access on a union | Fixed — member access and subscripts on `type` aliases, parameters and builtin unions report; a member every variant has is fine. |
| 3.4 | `nonlocal` write does not invalidate | Fixed — also writes through an alias and calls taking the narrowed object. |
| 3.5 | Arity through a `Callable` | Fixed — the annotation's parameters are all required; `*args` / `**kwargs` exempt (a first version broke `add(*args)` in the stress corpus). |
| 3.6 | `await` on a non-awaitable | Fixed for provably sync same-module calls and literals. **Narrower than proposed:** the checker types an un-awaited `async def` call as its declared return, so a bare name, a `Callable` call or an *imported* function stays permissive (a first version rejected `await run_crawl(...)` in four example apps). Tracked in `TYPE_SYSTEM_FRONTIER.md`. |
| 3.7 | Comparisons unchecked | Fixed — orderings, `in`, and union subscripts; `==` / `is` and mixed numerics never reported; a class with the dunder is trusted. |
| 3.8 | Re-confirmed 2026-09-01 holes | Fixed: subclass field redeclaration, positional-only by keyword, `for` over a non-iterable, undeclared slot writes, transitive `go`. Not fixed, by design: tuple-unpack arity on a fixed tuple, `int ** negative`, incompatible override (warn), use-after-`del` (warn) — documented limits. |
| 3.9 | Warn-level defaults | `nullable-use` defaults to `"error"`. This is the one deliberate narrowing on programs that ran; it is the alpha.7 plan, and it only became honest once the `and`-chain false positive (below) was fixed. |
| 4.1 | Relative imports break exhaustiveness | Fixed — `tyc-db` canonicalises the import against the importing package. |
| 4.2 | Facade `newtype` does not widen | Fixed — see 9.2. |
| 4.3 | `missing_return` on `try`/`for`/`while`-`else` | Fixed. |
| 4.4 | Smaller | All fixed: `ClassVar` split, `lazy let` lowering, constants-namespace lint on `class!` and inheriting subclasses, bare generic annotations. |
| 4.5 | Variance inference does not reach `impl` | Fixed — `impl[T]` methods and `frozen` fields (`tuple[T, ...]`) are observed; `@covariant` / `@contravariant` are stripped from emitted Python for top-level classes too. |
| 4.6 | Diagnostic completeness across modules | Root cause corrected — see 9.2. |
| 5 | `extend BUILTIN` receivers | Fixed on both surfaces, including package submodules and imported modules' own call sites under `tyc run`. |
| 6 | VM ↔ CPython | See 9.4. |
| 7 | Perf-gate flap, secret keywords, lockfile | Slack 10 ms; `OAUTH_*` keywords; lockfile refresh deferred (9.3). |

Along the way the stress corpus surfaced three more false positives that
the new checks would have turned into errors, all fixed before landing:
`if b is not None and b.val is not None: b.val + 1` did not narrow through
the `and` chain (the fp25 / t18 probes, previously warn-level);
`case int() as n` bound `n` to the whole subject union; and `add(*args)`
through a value callable was arity-counted. Every example project checks
clean; the only corpus units that flip to *rejected* are stress probes that
crash at runtime (`h*`, `s*`, `n*`, `t20`, and `fp25c`, whose
`len(a.b.c or [])` raises for a non-zero `c`), and eight previously
rejected units now check (`for`/`try`-`else`, the complex `match`, the
recursive alias, the with-chain f-string, fp25 / fp25a / t18).

### 9.2 Correction to §4.6

§4.6 attributed the missing `frozen_assign` through a facade to an
error-gated cross-module pass. That was wrong. The `pub *` aggregation
(`aggregate_pub_star_shapes` → `merge_pub_visible`) merged classes,
functions, interfaces and sealed unions but **dropped `newtypes`,
`frozen_classes`, `enums`, `type_aliases`, `class_param_variance`,
`gatherable_async_fns` and `hkt_param_names`**, so a class declared
`frozen` lost that through a facade and a facade-imported newtype was a
plain class — which is also the whole of §4.2. The "only when the module has no
other error" observation was a coincidence of the probes used. The merge
now carries every table (HKT names unfiltered, as they are not exported
names), with a unit test, and `examples/apps/16-shape-catalogue` covers the
facade, the relative imports and the re-exported `newtype` / `frozen` /
`enum` end to end on both surfaces.

### 9.3 Left for the maintainer

- **Lockfile refresh.** A semver-compatible `cargo update` pulls 98
  packages and adds the ICU segmenter crates; that is a dependency-wave
  decision (licence review, the vendored Ruff build), not a release fix.
- **Version bump and tag.** Nothing here bumps the workspace version;
  `v1.0.0-beta.1` is the maintainer's call, per §8.
- **Syntax freeze, compatibility policy, bot-PR backlog** (§8, item 8).
- **An extension does not travel through a `pub *` facade.** `from
  catalogue import describe` sees `describe` but not the `extend str`
  declared beside it; `from catalogue.text import describe` does. The new
  app documents the rule; carrying registries through facades is a small
  follow-up in `aggregate_pub_star_shapes`.
- **Coroutine-typed calls** (frontier): the un-awaited-coroutine typing
  that limits §3.6, and `make().slug()` on an `async def make`.

### 9.4 VM parity

Every §6 finding is closed and pinned by a VM unit test: a plain
`import pkg.sub` binds `sub` on `pkg` (native submodules included, and a
directory without `__init__.ty` is a namespace package); `map` over several
iterables zips them; `await` on a non-awaitable and `go` / `create_task`
outside a running loop raise CPython's errors; the modelled `math`, `re`,
`heapq`, `functools`, `contextlib`, `operator`, `collections` and `hashlib`
modules fill their gaps value-for-value against CPython 3.13 (`math` is now
complete, and `re` flags are honoured instead of ignored); and the pre-run
scan checks `module.attr` reads against what the VM's module actually
exports, so `re.purge` takes the compiled path with a `note:` instead of
dying mid-run. Residuals, all documented in `docs/vm.md`: `re` flags are
plain ints, `re.error` text is the Rust engine's, and `pbkdf2_hmac` on an
unknown digest raises `ValueError`. The extension-lowering gap that
surfaced while building the new app — an extension declared in a package
submodule, or called inside an imported module, raised `AttributeError`
under `tyc run` only — is fixed and covered by a pipeline test. The
differential gate over the full corpus passes with five pinned divergences:
`vm/p8.ty` turned out to be CPython disagreeing with itself on
`as_completed`'s set-ordered task start and is declared nondeterministic
instead, and the pydantic `02-structured-output-validation` unit now takes
the compiled path (the pre-run scan sees that the VM's `pydantic` exports no
`ValidationError`) rather than dying in the VM.

