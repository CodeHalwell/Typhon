# Changelog

All notable changes to Typhon are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) loosely; the
canonical phase-by-phase status lives in `docs/roadmap.md`.

## 0.3.0

Six new language features for everyday Python annoyances, a coordinated
sweep that closes every remaining open finding from the May 2026 stress
campaigns (O2–O29), and cross-platform install support: pre-built `tyc`
binaries for **Linux (x86_64 + aarch64)** and **Windows (x86_64)** join
the existing **macOS (Apple Silicon + Intel)** matrix.

### Added — language

- **`newtype Name = Base` — nominal aliases over base types.**
  TypeScript-style nominal aliasing that keeps same-shaped primitives
  (`UserId` vs `PostId`, `USD` vs `EUR`, internal vs external IDs)
  from being silently swapped. Asymmetric by design: a `UserId` flows
  into an `int`-typed slot (the runtime value *is* an `int`), but a
  bare `int` requires an explicit `UserId(x)` constructor call to
  satisfy a `UserId`-typed target. Compiles to a zero-cost
  `typing.NewType` call. New `tyc::newtype_violation` diagnostic.
- **`freeze let X = expr` — deep-immutable bindings.** Closes the gap
  left by `let`, which only locks the binding name and not the
  underlying value. `freeze let` wraps the RHS in
  `__typhon_freeze__(...)` from `typhon_runtime.freeze`, which
  recursively converts `list → tuple`, `dict → MappingProxyType`,
  `set → frozenset`, and descends into nested values. Anything without
  a clean immutable equivalent (file handles, sockets, generators,
  non-frozen dataclasses) raises `TypeError` at startup rather than
  via a confusing downstream mutation.
- **`pub` modifier for module-level visibility.** When a module
  declares at least one `pub` name, desugar synthesises a top-of-file
  `__all__ = [...]` list so `from foo import *`, Sphinx autoapi, IDE
  re-export filters, and the checker's re-export inference all see
  the public surface — no hand-maintained `__all__` lists required.
  Composes with the existing keyword stack (`pub let`,
  `pub frozen class`, `pub model`).

### Added — diagnostics

- **`tyc::blocking_in_async`.** Catches direct calls to known-blocking
  stdlib functions (`time.sleep`, `requests.get`, `socket.recv`,
  `subprocess.run`, `input`, `urllib.request.urlopen`, …) inside an
  `async def` body. A blocking call halts the entire event loop until
  it returns; the diagnostic suggests `asyncio.to_thread(...)` /
  `loop.run_in_executor(...)`. Suppressed inside `unsafe:` regions.
- **`tyc::resource_not_managed`.** Flags bare assignments of
  context-manager-returning calls (`open`, `socket.socket`,
  `sqlite3.connect`, `tempfile.*`) that aren't wrapped in a `with`
  statement, where deterministic cleanup matters and the runtime
  would otherwise leave teardown to the garbage collector. Severity
  defaults to `warn`; controlled by `[strictness] resource-not-managed`.
- **`tyc::div_by_zero_literal`.** Catches `x / 0`, `x // 0`, and
  `x % 0` at compile time when the divisor is a literal — `0`, `0.0`,
  `-0`, `-0.0`, or any unary-negated form. Pure constant-fold lint
  with zero false positives. Flow-sensitive analysis (`if d != 0:`
  guards on runtime values) is deliberately out of scope.
- **`tyc::unsafe_value_leak`** (O14). A `return x` outside the
  `unsafe:` block where `x` was declared, against a function with a
  concrete annotated return type, now fires a dedicated diagnostic
  with help text pointing at both workaround forms (`let x: T = …`
  inside the block, or `let typed: T = x` outside).
- **`tyc::pattern_shadows_outer`** (O10). `case Wrap(value):` against
  an outer `let value` now fires a dedicated diagnostic instead of
  the misleading `tyc::immutable_assign`. Suggests renaming the
  capture — the right advice for the Rust/OCaml/Scala intuition every
  newcomer brings to `match`.
- **`tyc::extend_builtin`** (O23). `extend list[int]:` (parametric
  target) now fires a dedicated diagnostic naming the parametric
  shape, rather than the confusing downstream
  `tyc::impl_unknown_class` cascade.

### Added — install + release

- **Linux pre-built binaries.** `tyc-<version>-x86_64-unknown-linux-gnu.tar.gz`
  and `tyc-<version>-aarch64-unknown-linux-gnu.tar.gz` ship on every
  release tag. The same `install.sh` now detects `Linux` from
  `uname -s` and resolves the matching tarball; the macOS path is
  unchanged (Gatekeeper quarantine xattr clearing is skipped on
  Linux). Built on `ubuntu-22.04` for broad glibc compatibility.
- **Windows pre-built binaries.** `tyc-<version>-x86_64-pc-windows-msvc.zip`
  ships on every release tag, alongside a new `install.ps1`
  PowerShell installer. The script downloads the zip, verifies its
  SHA-256, extracts to `%LOCALAPPDATA%\Programs\Typhon` by default,
  and adds the directory to the user-level `PATH` via
  `[Environment]::SetEnvironmentVariable`. Supports `--Version` and
  `--InstallDir` flags + `TYPHON_VERSION` / `TYPHON_INSTALL_DIR`
  env vars.
- **Release workflow.** `.github/workflows/release.yml` now runs a
  five-job matrix (macOS Apple Silicon + Intel, Linux x86_64 +
  aarch64, Windows x86_64) and uploads tarballs + the Windows zip +
  a combined `SHA256SUMS` file to the GitHub Release. Linux
  aarch64 cross-compiles from Ubuntu via the `aarch64-linux-gnu-gcc`
  toolchain.

### Findings sweep — every open finding closed (O2–O29)

This release coordinates the two final findings-closure branches
(`claude/findings-documentation-review-HhuVH` and
`claude/finish-open-findings-dmLv6`). Across nine stress campaigns
(May 17–21 2026, ~600 hand-written `.ty` programs, ~120 distinct
findings) the **Open** column on `docs/findings.md` is now empty
for the first time since campaign tracking began.

- **`tyc fmt`** (O12, B9) — five new PEP 8 rules wired into the
  in-process pass: space after `:`, spaces around `->`, space after
  `,`, single-space around binary `+` / `-` and top-level `=`, two
  blank lines before top-level `def` / `class` / `async def`. The
  O12 repro `def    f(  x:int,y:int)->int:` reformats end-to-end.
- **TypedDict-style dict literals** (O15). `let alice: User = {"id":
  1, "name": "Alice"}` against a registered class shape now matches
  keys against fields and flows each value under the declared field
  type before falling through to the ordinary `dict[K, V]` path.
- **Sized-style Protocols on built-ins** (O16). `list`, `dict`,
  `tuple`, `set`, `str`, `bytes`, `range`, `frozenset`, `bytearray`
  now satisfy a user-declared Protocol whose declared methods are
  all common dunders (`__len__`, `__iter__`, `__getitem__`,
  `__contains__`, `__eq__`, `__bool__`, …).
- **Inline `?`** (O17). `Ok(add(parse(s)?, parse(t)?))` compiles. A
  new `expand_inline_question_ops` pre-pass lifts every mid-line `)?`
  into a `__typhon_qi_N__` temp + propagation guard before the
  existing end-of-line pass runs.
- **`while` narrowing** (O2). `Stmt::While` now applies test-implied
  narrowings to the body via the same path the `if` checker uses;
  the linked-list iterator idiom
  `while cur is not None: total += cur.value; cur = cur.next`
  type-checks.
- **`tuple[T, ...]`** (O3) — resolves to an internal
  `tuple_variadic[T]` head that the unifier accepts against any
  fixed-length tuple literal whose elements are all assignable to
  `T`, including `()`.
- **Cyclic type aliases** (O4) — surface `tyc::cyclic_type_alias`
  once, then rewrite the alias body to `Any` so subsequent uses fall
  through silently instead of cascading into `type_mismatch` errors.
- **`class! Foo(Exception)`** (O5) — synthesises
  `def __init__(self, *args, **kwargs)` calling `super().__init__`
  when the body has no annotated fields, so `raise AppError("boom")`
  reaches the parent constructor.
- **Generator `return`** (O6) — `return` and `return value` inside a
  generator function body type-check against the declared
  `Iterator[T]` / `Generator[Y, S, R]` return type.
- **`tyc migrate` nullable forward-refs** (O21) — `Optional["Item"]`
  now becomes `"Item?"`, not the previously unparseable `"Item"?`.
- **`tyc migrate` Union → `T?`** (O22) — rewrites `Union[T, None]`
  (and `Union[None, T]`, and the `typing.Union[...]` qualified form)
  to `T?` and drops the import; PEP 604 pipe-union fallback for
  multi-arm unions so imports are never left dangling.
- **`tyc explain --list`** (O25) — now prints every diagnostic code
  the binary knows about; the "not yet implemented" message for
  unknown codes is gone.
- **VM `Result` repr** (O24) — `Value::ResultOk` / `Value::ResultErr`
  match CPython's dataclass default (`Ok(value=20)` /
  `Err(error='oops')`); single-quote preference matches Python's
  `repr`. `tyc run` and `tyc run --compile` now produce
  byte-identical stdout for Result-bearing programs.
- **REPL prompts when piped** (O26) — `tyc repl` checks
  `stdin.is_terminal()` and skips the `>>> ` / `... ` prompts when
  stdin is piped.
- **`tyc build --no-sync`** (O29) — new flag (and `TYC_NO_SYNC=1`
  env var) skips the `uv sync` step while still merging
  `pyproject.toml`. Stress harnesses and REPL-like iteration on
  tmp projects no longer pay the per-invocation reprovision cost.
- **`MatchSequence` bracket recovery** (O27) — `case [a, b]:`
  re-emits as `[a, b]` rather than the default `(a, b)` by peeking
  at the original `TextRange` to recover the bracket choice.
- **Pipe corner cases** (O28) — `5 |> (lambda x: x * 2)()` and
  `(1 |> add(2)) |> add(3)` both compile. New
  `expand_pipes_in_subexpressions` pre-pass recursively expands
  pipes inside every balanced `(...)` group before the line-level
  pass runs.

### Tests

- 4 new preprocess unit tests for `newtype` lowering (`Name =
  NewType(...)` round-trip, generic bases, fmt round-trip,
  indented-form rejection).
- 7 new unit tests for `tyc::div_by_zero_literal` covering `/`,
  `//`, `%`, float zero, negated zero, and `unsafe:` suppression.
- New regression tests for `tyc::blocking_in_async`,
  `tyc::resource_not_managed`, `tyc::unsafe_value_leak`,
  `tyc::pattern_shadows_outer`, and `tyc::extend_builtin`.
- `tyc fmt` regression suite covers each of the five new PEP 8
  rules end-to-end.
- All B9-bucket pre-existing-bug repros under
  `stress/round-2026-05-21/` build clean.

## 0.2.5

Second editor-UX pass on the 0.2.4 LSP work. Closes the three
remaining "doesn't match what VS Code does for Python" gaps in
the original report:

### Added

- **Module-path tokens in `from X.Y import Z`.** Dotted module
  paths in both `from` and bare `import` statements now emit a
  `namespace` token per segment, with `defaultLibrary` applied for
  stdlib roots. Previously the resolver only emitted a token for
  `Z` (the binding), leaving `X.Y` uncoloured — visually the most
  prominent part of every import line. Each segment is its own
  token so themes (and future "go to import" code actions) can
  target sub-segments.
- **Shape-aware kwarg colouring.** Call sites like
  `Agent(client=…, model=…)` now classify each kwarg name against
  the callee's real signature:
  - `property` (orange) when the kwarg names a declared parameter.
  - `parameter` (yellow) when the callee declares `**kwargs` and
    the kwarg isn't on the explicit list.
  - No token (white) when the kwarg is unrecognised and the callee
    has no catch-all — a visible cue that something's off without
    us claiming a hard diagnostic.

  The LSP pre-resolves callee signatures by batch-querying the
  introspection cache for every top-level imported class /
  function in the open buffer. Signature parsing is tolerant of
  nested-tuple defaults (`a=(1, 2)`), quoted commas
  (`sep=', '`), positional-only `/`, kw-only `*`, and the
  conventional `self` / `cls` first parameter.
- **Class docstring fallback to `__init__`.** When a class body
  carries no docstring (common pattern when the author wrote the
  documentation on `__init__` instead), hover now falls back to
  `__init__.__doc__` so the popover still shows parameter
  documentation. Without the fallback the hover for classes like
  `agent_framework.Agent` degraded to "📦 from …" with no Markdown
  body.
- **RST inline cleanup in hover docstrings.** Two of the most
  common Sphinx markup shapes are normalised to Markdown before
  rendering: ``\`\`code\`\``` (RST double-backtick) becomes
  ``\`code\``` (Markdown), and Sphinx role markup like
  `:class:\`Foo\``, `:func:\`bar\``, `:meth:\`baz\`` has the role
  stripped, leaving just the backticked symbol. Pylance does the
  full Sphinx → HTML render; this is the 80/20 that catches the
  shapes appearing in most third-party docstrings.
- **Signature truncation cap raised** from 400 → 1024 characters
  so Pydantic / SQLAlchemy / Django model constructors with 20+
  typed parameters render their full signature instead of being
  dropped entirely.

### Tests

- 10 new tests in `tyc-lsp::semantic`:
  - Module-path emission: `from foo.bar import Baz`,
    `from os.path import join` (stdlib modifier), dotted
    `import foo.bar.baz`.
  - Kwarg classification: real-param → `property`,
    `**kwargs` catch-all → `parameter`, unknown → no token.
  - Signature parser: basic param names, `**kwargs` detection,
    nested-default commas, `self` / `cls` skip.
- 2 new tests in `tyc-lsp` for `render_docstring`: RST
  double-backtick code, Sphinx role directive stripping.

## 0.2.4

Editor developer-UX pass: semantic-token colouring for the LSP and
structured docstring rendering in hover. The two together close the
gap users hit on their first day with Typhon — "which of these
imports is from my project vs the library?" and "what arguments
does this take and what do they mean?".

### Added

- **LSP semantic tokens.** `textDocument/semanticTokens/full` is now
  served. Walks the resolved module and parsed AST to emit a token
  stream tagged with LSP-standard types (`class`, `function`,
  `method`, `property`, `parameter`, `variable`, `namespace`) and
  modifiers (`declaration`, `defaultLibrary`). VS Code themes apply
  colours automatically — stdlib imports get the `defaultLibrary`
  shade (Python convention: muted blue), third-party / project
  imports get the usual class/function colours, and method calls
  (`agent.run()`) are coloured distinctly from property reads
  (`agent.name`). The legend is published in the server capabilities
  so the indices stay stable across releases.
- **Structured docstring sections in hover.** `render_docstring` now
  detects Google / NumPy / Sphinx-style sections (`Args:`,
  `Parameters\n----------`, `:param name:`) and re-renders them as
  Markdown headers + bullets. Parameter lines (`name: desc`,
  `name (type): desc`, `name : type\n    desc`) become
  `- **name** — desc` bullets, so the hover popover shows the
  parameter list with descriptions instead of a wall of indented
  text. Free prose, examples, and unrecognised sections are
  preserved verbatim.

### Tests

- 7 new tests in `tyc-lsp::semantic` covering stdlib /
  third-party / project token classification, declaration-modifier
  emission on class + function decl sites, method-vs-property
  detection in `Expr::Attribute`, and LSP delta-encoding
  correctness.
- 5 new tests in `tyc-lsp` for `render_docstring`: Google `Args:`,
  NumPy `Parameters\n----------`, Sphinx `:param X:`, recognised
  `Examples:` section, and the pass-through for unstructured
  prose. The existing PEP 257 indent-strip test is updated to
  assert on the new structured output.

## 0.2.3

UX polish on the arity diagnostic landed in 0.2.2 plus full hover
developer-UX for third-party imports in the LSP. Lands as one
release: the diagnostic naming and the hover preview are the same
"tell me what's wrong / tell me what this thing is" story.

### Added

- **`tyc::missing_argument` diagnostic.** When the checker can
  pinpoint *which* parameter wasn't supplied (constructor or
  free function), the new code fires instead of the count-based
  `tyc::arg_count` — so `Agent(name=..., tools=...)` now reads
  `missing required argument to 'Agent': 'client'` and a one-line
  `help: supply 'client' when calling 'Agent'`, instead of the
  misleading "expected 1, got 4". Multiple missing names render as
  ``` `a`, `b` ``` with the plural form. `tyc::arg_count` still
  fires for shape mismatches that can't be reduced to a missing-name
  list — `missing_required_fields` and `missing_required_params`
  detect positional+kwarg double-binding, too-many-positionals, and
  `*iter` / `**dict` unpacks and return empty so the count-based
  diagnostic stays the source of truth for those cases.
- **LSP hover docs for third-party imports.** Hovering an imported
  class or function in VS Code (or any LSP-aware editor) now shows
  the source module, the recovered signature in a fenced `python`
  code block, and the runtime docstring rendered as proper Markdown
  — pulled from the same venv-introspection cache that already
  powers completion. Project / stdlib symbols fall through to the
  existing kind-only hover.
- **Responsive prewarm on document open / change.**
  `check_and_publish` spawns a detached task that introspects every
  third-party import in the open buffer the moment the file is
  parsed, so the cache is hot by the time the user hovers. The
  first hover used to wait on the subprocess (30–100 ms typical);
  subsequent hovers hit the cache. The prewarm never blocks
  diagnostics publishing, and is debounced by document version so
  rapid typing doesn't queue redundant blocking-thread work.
- **Markdown rendering for hover.** Hover now publishes
  `MarkupContent { kind: Markdown, … }` instead of the older
  `MarkedString::String` shape so signatures appear in fenced
  code blocks and docstrings render as paragraphs.
- **Multi-line docstrings.** The Python introspection script now
  returns the full docstring (up to 4 KB) instead of the first
  line; the LSP strips PEP 257 indentation and trims surrounding
  blanks before rendering. Caps the visible body at 40 lines with
  an explicit truncation marker so module-level docstrings (numpy,
  pandas, sklearn) don't flood the popover.
- **Off-runtime introspection in hover.** The hover handler runs
  the cold-path `cache.members()` call through
  `tokio::task::spawn_blocking` so a worst-case 5-second timeout
  can never stall the async runtime. The prewarm makes cold hits
  rare; this is the safety net for the case where the prewarm
  hasn't completed yet.
- **Poisoned-mutex recovery on hover + prewarm.** Both paths now
  recover poisoned `IntrospectionCache` mutexes via
  `into_inner()`, matching the completion path. A prior panic no
  longer permanently disables hover / prewarm for the rest of the
  session.

### Tests

- The three `tyc-db` cross-module tests and the four `tyc-types`
  arity tests that asserted on "wrong number of arguments" now
  match the new wording (and assert on the specific missing name).
- 4 new regression tests in `tyc-types` covering the
  `missing_argument` guards: positional+kwarg double-binding (ctor
  + free fn), too-many-positionals (ctor + free fn with kw-only
  required).
- 5 new tests in `tyc-lsp` covering `render_docstring` (PEP 257
  strip, blank-line trim, 40-line cap, empty input) and `sig_tail`
  (prefix strip, unknown-shape pass-through).
- One `tyc-types` arity test on `def add(a, b)` accepts either
  wording to stay forward-compatible with future diagnostic
  refinements.

## 0.2.2

Third-party signature recovery via venv introspection. The flagship
gap this release closes: a project that imports a class from an
unstubbed PyPI package (`from agent_framework import Agent`) and
calls it missing a required argument (`Agent(name="x", tools=[])`
without the required `client` kwarg) passed `tyc check` and
`tyc build` clean in 0.2.1, then crashed at runtime with
`TypeError: Agent.__init__() missing 1 required positional
argument: 'client'`. The checker had no signature for `Agent`
because no `.dty` stub was authored, so the callable degraded to
`Type::Unknown` and the arity check at
`tyc/crates/tyc-types/src/lib.rs:6061` (which only fires for
project-local classes) was skipped.

0.2.2 closes the loop by shelling to the project's
`.venv/bin/python` (or a fallback `python3` on PATH), asking
`inspect.signature` for the real parameter list of every public
class and free function in each declared dependency, and folding
the result into the same `ModuleShapes` registry that
`tyc-db::build_external_shapes` already consumes. No changes to the
checker itself — once the shape is in the registry, the existing
cross-module constructor / function arity path fires identically
to in-project calls.

### Added

- **`tyc/src/venv_signatures.rs`: venv-driven signature
  introspection for the type checker.** Walks every `.ty` file's
  `import` / `from ... import ...` / `lazy import` statements to
  collect dotted module names, then runs an embedded Python helper
  in the project venv that emits structured per-parameter info
  (name, kind, has-default) for every public class and free
  function. The Rust side converts that into an `InterfaceShape`
  (for classes — each `__init__` param becomes a field, with
  defaulted params populating `field_defaults`) or an `ArityInfo`
  (for free functions — kw-only / positional / `*args` / `**kwargs`
  preserved), and merges the result into the project shape
  registry. `tyc check` and `tyc build` consume the enriched
  registry; no changes were needed in `tyc-types` or `tyc-db`.

### Safety rails

- Only modules whose top-level package is listed in
  `[dependencies]` / `[dev-dependencies]` (or maps to a declared
  distribution via `.dist-info/top_level.txt`) are introspected.
  Stdlib (`os`, `json`, `collections`) and project modules stay on
  their existing resolution paths — no Python subprocesses for
  ordinary stdlib usage.
- Classes whose `__init__` declares `*args` or `**kwargs` are
  deliberately skipped. False positives on permissive Python APIs
  (every extra kwarg firing `tyc::unknown_kwarg`) would be worse
  than the existing miss.
- All failures (no venv, no Python on PATH, import-time exception,
  5-second introspection timeout) silently no-op. Worst case is the
  prior 0.2.1 behaviour — the runtime catches what the checker
  couldn't.
- Subprocess `current_dir` is pinned to the project root so the
  same `tyc check` from any subdirectory produces the same shape
  registry. Same reproducibility contract that already applies to
  `tyc::unknown_module`.
- The introspection result is cached per `VenvSignatures` instance
  keyed by dotted module name; one subprocess per module per
  `tyc check` invocation.

### Tests

- 10 new unit tests in `tyc::venv_signatures` covering the
  shape-conversion logic (required kw-only params, `**kwargs`
  bail-out, `*args` bail-out, positional+kw-only mix, `*args` /
  `**kwargs` on free functions, dotted-name validation, allow-list
  gating, and import-statement extraction across every Typhon /
  Python form).
- One new integration test in `tyc::commands::check::tests`
  (`check_introspects_third_party_class_constructor_arity`) that
  builds a fake third-party package, runs the introspection
  helper, and asserts the recovered shape models the original bug.
  Skips silently when no Python 3 is on PATH so CI runners without
  Python don't fail the suite.

### Caveats

- An author who genuinely needs the missing-arity check on a
  class with `**kwargs` should write a `.dty` stub — the stub
  declares the real surface and the arity check fires normally.
  See `docs/guides/08-…-stubs` (TBD).
- Python imports can have side effects. Modules that touch the
  network or sleep at import time would now do so during
  `tyc check`. The 5-second timeout caps the cost; users on
  pathological packages can declare the dep out of
  `[dependencies]` to skip introspection.

## 0.2.1

Correctness fixes surfaced by the
`stress/round-2026-05-20-exploration` corpus (~80 fresh `.ty`
programs across syntax edges, I/O, ML/NumPy, AI/LLM clients, agents,
HTTP servers, and SDK clients).  Documented per finding under
`stress/round-2026-05-20-exploration/FINDINGS.md`.

The flagship fixes in this point release are two **silent
wrong-output / wrong-rejection** bugs that the prior round's stress
corpus shook out:

- A pathological hole in the emitter precedence table that let
  `(a + b).upper()` round-trip as `a + b.upper()` — same semantics
  problem as the walrus bug fixed in 0.2.0, but with attribute /
  call / subscript / boolean-op as the outer context instead of an
  arithmetic BinOp.  Surfaced live in `pathlib.Path / "x"` chains and
  a NumPy least-squares reduction (`(dx * dy).sum() / (dx * dx).sum()`).
- The type checker refused to accept `None` or `T?` values against an
  `impl X: def f(self, p: T?)` parameter — the method's per-param
  types were never recorded and fell back to a `Vec![Type::Unknown; arity]`
  shape, which collided with the call-site
  nullable-into-non-nullable guard.

### Fixed

- **`tyc-emit`: parentheses preserved around non-atomic expressions
  used as the receiver of postfix ops.**  `Attribute`, `Call`,
  `Subscript`, and `BoolOp` emit arms now route the inner expression
  through a `needs_paren_for_postfix` guard.  Adds wrapping around
  `BinOp` / `BoolOp` / `UnaryOp` / `Lambda` / `IfExp` / `Compare` /
  `Named` (walrus) / `Await` / `Yield` / `YieldFrom` / `Starred` /
  `Generator` whenever they appear in those four contexts.  Also
  parenthesises a lower-precedence `BoolOp::Or` child inside a
  `BoolOp::And` parent.  (FINDINGS E1 — silent precedence bug.)
- **`tyc-types`: `MethodSig` now records per-parameter types.**
  `collect_class_shape` populates `MethodSig::param_types` from the
  declared annotations (stripping the implicit `self` / `cls` slot
  to mirror `arity_info`).  The call-site resolution path for
  `instance.method(...)` reads these into the `Type::Function::params`
  it returns, so `instance.method(value)` is now type-checked against
  the real declared parameter types instead of `Vec![Type::Unknown; arity]`.
  Fixes `impl X: def f(self, p: T?)` rejecting `None` and `T?`
  arguments.  (FINDINGS E2 — impl `T?` params.)
- **`tyc-types`: `self` inside an `impl` block carries the user-facing
  class name, not the desugarer's `__typhon_impl_X` pseudo-class.**
  The pseudo-class is an intermediate shape that the merge pass folds
  into the real class; previously the checker walked the methods
  first and `return self` against `-> X` failed with
  `expected X, found __typhon_impl_X`.  Stripping the prefix at
  `self`-receiver binding time gives the user the diagnostic surface
  they expect.  (FINDINGS E3 — broke `__enter__` / context-manager
  patterns.)
- **`tyc-types`: call-site `func_type` is unwrapped through type
  aliases.**  Calling through a transparent
  `type Handler = Callable[[Req], Resp]` alias now resolves to the
  underlying `Type::Function`, so the call's return type is `Resp`
  rather than `Handler`.  Middleware / decorator / handler-pipeline
  patterns work without inlining the `Callable[...]`.  (FINDINGS E4.)
- **`tyc-types`: user-defined dunders take precedence over the
  numeric-coercion table in `BinOp` inference.**  `Vec2(...) * 5.0`
  for `impl Vec2: def __mul__(self, scalar: float) -> Vec2` now
  resolves to `Vec2`, not `Float`.  Also reaches for the reflected
  dunder (`__radd__` / `__rmul__` / …) when the LHS is a primitive
  and the RHS is a user class.  (FINDINGS E5.)
- **`tyc-types`: exhaustiveness recognises positional class
  patterns.**  `case Leaf(value):` / `case Branch(left, right):`
  (positional captures of every declared field) now count as total
  matches for the named variant, so `missing_return` no longer fires
  on legitimately-total `match` statements over recursive sealed
  unions.  (FINDINGS E6 / R3.12 follow-up.)
- **`tyc-analyse`: comptime f-strings.**  `comptime let TITLE: str = f"{APP} v{MAJOR}.{MINOR}"`
  now evaluates at build time as long as every interpolation is
  itself a comptime constant.  Format specs and conversion flags
  (`!r` / `!s` / `!a` / `:>5`) remain unsupported and surface as
  explicit comptime errors instead of being silently dropped.
  (FINDINGS E7.)
- **`tyc-types`: calling a `Type::Class(...)` whose shape we don't
  have degrades to `Unknown` instead of treating it as a
  constructor.**  Catches the `self.linear(x)` shape where `linear`
  is an instance field typed as a foreign class (e.g. `torch.nn.Linear`) —
  the call is most likely invoking `__call__`, not constructing a
  fresh instance.  Pre-fix the imported class's "constructor" result
  would leak into `mut`-bound rebinds; post-fix the result is
  `Unknown` and the surrounding annotation drives assignability.
  (Knock-on fix surfaced by E3.)

### Tests

- 7 new regression tests in `tyc-types` covering E2, E3, E4, E5,
  E6 (positional / shorter-positional class patterns), and the
  dunder-rejects-arg-type-mismatch case added during PR-#87 review.
- 3 new regression tests in `tyc-analyse` covering E7 (the happy
  path, the format-spec rejection, and the
  list/tuple/dict-interpolation rejection added in PR-#87 review).
- 6 new emit round-trip tests in `tyc-emit` covering the
  paren-stripping shapes (`(BinOp).attr`, `(BinOp)[idx]`,
  `(ternary).attr`, `(lambda)(arg)`, `(Or).And`,
  `(path / "x").method(...)`).

### Documented in `stress/round-2026-05-20-exploration/`

A new stress round runs the full eight-domain corpus against this
release.  The remaining open issues (E8 class-const slot
descriptors, E9 `?` in subexpressions, E10 `for`-target rebinding,
E11 per-build venv DX, and the typed-SDK `dict[str, object]`
variance issue) are catalogued in
`stress/round-2026-05-20-exploration/FINDINGS.md` for the next
round.

## 0.2.0

Constructor / method arity safety. The flagship bug this release
catches: a class declared with `class ApiClient: api_key: str` that
the user instantiates as `ApiClient(base_url="…")` — passing `tyc
check` and `tyc build` in 0.1.6, crashing at runtime with `TypeError:
missing 1 required positional argument`. v0.2.0 surfaces the same
bug at check time, before the build ever runs.

### Review-driven fixes (post initial v0.2.0 RC)

- Bare-import dotted access now uses qualified `module.class` names
  internally, so two imports exporting the same class don't collide
  in the lookup table. Diagnostics also surface the qualified name
  (`clients.ApiClient`) for disambiguation.
- Same fix applied to imported free functions.
- `model X:` declarations correctly require every non-defaulted
  field at construction even when defaults appear earlier in the
  body — `ArityInfo` now carries a per-param `required_positional`
  flag rather than relying on the "all required come first" Python
  convention.
- `impl X:` / `extend X:` blocks contributing fields with defaults
  now merge `field_defaults` correctly; previously they were treated
  as required at construction.
- `[project] src = "app/src"` (nested src) now derives dotted names
  correctly — the basename of the configured src dir is what
  `path_to_dotted` actually needs.
- The post-construction audit no longer fires on `return c.field` or
  `f(c.field)` (receiver-of-attribute-access isn't an instance
  escape) and no longer emits duplicate diagnostics for repeated
  names like `return c if cond else c`.
- `setattr(obj=c, name=…, value=…)` (kwarg form) now drops audit
  tracking like the positional form does.
- `infer_expr_readonly` handles `Expr::Call` so chained calls like
  `clients.ApiClient(…).url(…)` resolve the receiver type for
  method arity checks.
- The project shape registry is now `Arc<HashMap<…>>` end-to-end so
  per-file `ExternalShapes` snapshots are O(1) refcount bumps
  instead of O(modules) deep clones.

### Added

- **`tyc::arg_count` now fires on class constructors.** The
  auto-generated `__init__` of `class` and `model` declarations is
  arity-checked at every call site. Fields with no `= default` are
  required; fields with a default are optional. `T?` without an
  explicit `= None` is still required (Typhon doesn't auto-inject the
  default), matching the emitted dataclass's runtime semantics.
- **`tyc::arg_count` now fires on `impl` methods.** Method signatures
  carry full `ArityInfo` (param names, defaults, `*args`/`**kwargs`)
  instead of a single arity count. `user.greet()` is flagged when
  `greet` declares a required `prefix: str` parameter.
- **Cross-module arity checks.** `from foo import ApiClient`
  followed by `ApiClient(…)` arity-checks against `foo`'s exported
  shape. `import foo as f` followed by `f.ApiClient(…)` does too via
  the new `Type::Module(name)` modelling. Both `.ty` source and
  `.dty` stubs flow through a project-wide shape registry built once
  per invocation. Works in `tyc check`, `tyc build`, and the LSP.
- **Salsa-cached shape extraction.** A new
  `tyc_db::module_shapes_query(file)` salsa-tracked query caches
  per-file shape extraction. The LSP backend keeps a per-project
  `HashMap<dotted_name, SourceFile>` so handles survive across
  keystrokes; a keystroke in one file only re-runs extraction for
  that file.
- **`tyc::missing_field_init` post-construction audit.** Catches the
  `X.__new__(X)` / `object.__new__(X)` bypass-construction pattern:
  if the instance escapes the function (return / call argument)
  without every required field assigned, the audit fires. Dropped
  conservatively on `setattr`, on `obj.method(…)` calls, and inside
  `unsafe:` regions.
- Public API in `tyc-types`: `InterfaceShape`, `MethodSig`,
  `ArityInfo`, `ModuleShapes`, `ExternalShapes`, plus
  `extract_module_shapes` and `check_module_with_imports`. Downstream
  tools building on the type-check pipeline now have a stable
  surface for cross-module checks.

### Changed

- `Type::Module(String)` joins the type enum, exposed for the bare-
  import attribute-access path.
- `tyc-db` re-exports `ModuleShapes` so CLI and LSP callers don't need
  to depend on `tyc-types` directly.
- The check pipeline's `Type::Function` arm now consults
  attribute-callee arity info before falling through to the
  permissive shape, closing the long-standing method-arity gap.

### Limitations

- Dotted-attribute annotations (`let c: f.Cls = …`) don't yet resolve
  to the foreign class shape. The constructor call itself
  arity-checks, but the binding lands as `Type::Unknown`. Workaround:
  use `from foo import Cls` or drop the annotation.
- The post-construction audit only flags return statements and
  function-call arguments as escapes; container-literal storage
  (`return [c]`) and outer-scope assignment aren't tracked.
- The audit is intra-procedural. A method that genuinely initialises
  fields suppresses the diagnostic (correctly); a method that
  doesn't will also suppress it (false negative).
- The audit doesn't track subclass field requirements separately.

### Migration notes from 0.1.6

The new arity checks are strict by default and may surface
pre-existing latent bugs in your codebase — that's the point. Two
patterns commonly need attention:

1. `Foo()` calls where `Foo` has required fields. Either add the
   missing arguments or give the fields defaults (`field: str = ""`).
2. Method calls that previously passed under the permissive shape.
   If a method signature changes (e.g. you add a parameter), every
   call site is now flagged immediately.

There are no behavioural changes at runtime — the emitted Python is
identical to 0.1.6. Only `tyc check` / `tyc build` reject more
programs.

## 0.1.6

See the [v0.1.6 release notes](https://github.com/CodeHalwell/Typhon/releases/tag/v0.1.6).

Phase 5 — Interop and developer experience: `plain class` marker,
`Enum`/`Flag`/`ABC` auto-skip plus configurable
`[emit] skip-decoration-bases`, `class-default` validation,
`or`/`and` truthy-union typing, generator→`Iterable` conformance,
`tyc explain <code>` / `tyc cheatsheet`, upgraded `tyc init`,
`.py`-in-`src/` copy-through, `tyc build --check`,
`tyc::contains_secret_literal`, miette `url(...)` deep-links,
`tyc fmt` wrapping `ruff format`, `tyc debug --break <ty>:<line>`.

## Earlier

For history before 0.1.6 see `docs/roadmap.md` and the corresponding
GitHub release tags.
