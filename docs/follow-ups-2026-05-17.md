# Follow-ups after the May 2026 completion sprint

This document tracks what landed in this branch
(`claude/assess-project-completion-gUdOP`), what was deliberately deferred,
and the rationale.  Read alongside `docs/roadmap.md` for the canonical
phase status.

## What landed

| Area | Change |
|---|---|
| Syntax | Module-level `lazy let NAME: T = expr` lowers to `lazy_val(lambda: expr)`; class-body `lazy let` lowers to `@cached_property`. Both round-trip through `tyc fmt`. |
| Syntax | `extend BUILTIN:` is rejected at preprocess time with a dedicated `tyc::extend_builtin` diagnostic; user-defined classes still flow through `impl`-merge. *(Update: built-in extensions have since landed — `extract_builtin_extensions` lowers each method to a free function and rewrites annotated call sites. The `validate_extend_usage` validator is now a no-op kept for back-compat with the diagnostic enum.)* |
| Types | `unsafe:` blocks now bump `Checker::unsafe_depth` via preprocess line metadata; type-mismatch, nullable-use, interface-isinstance, wrong-arg-count, not-callable, and non-exhaustive-match diagnostics are dropped inside the block. Errors on lines outside the block are unaffected. |
| Types | `Type::TypeVar(name)` replaces `Type::Any` for PEP 695 type parameters in signatures; call-site `bind_typevars_and_substitute` infers bindings (recursively, with conflict-widening) and substitutes them in the return type. |
| CLI | `tyc trace` reads `.py.map` sidecars emitted by `tyc build` and rewrites `File "…/foo.py", line N` traceback entries to point at the original `.ty`. |
| CLI | `tyc profile` post-processes the build output with a `@__typhon_profile_record` decorator on every top-level function plus a generated `typhon_profile.py` helper that flushes call counts and total wall-clock time to `typhon-profile.json` on `atexit`. |
| CLI | `tyc migrate` converts typed Python (`.py`) to Typhon (`.ty`): `Optional[T]` / `T \| None` → `T?`, module-level annotated assigns gain `let`/`mut` (let unless later reassigned), `@dataclass` decorators and the `dataclass` import are dropped. |
| LSP | `Backend` caches the latest text per URI on `did_open`/`did_change` so hover and go-to-definition can resolve without re-fetching from the editor. |
| LSP | `textDocument/hover` returns the binding kind + mutability rendered in markdown; `textDocument/definition` jumps to the resolver's recorded declaration span. Both rely on a new `ResolvedModule::symbol_at_offset` query. |
| Stubs | `tyc check --stubs` now diffs each `.dty`'s public API against its sibling `.ty` (or `.py`) implementation via a new `tyc_emit::compare_modules`; mismatches surface as `tyc::stub_mismatch` diagnostics. Private names (leading underscore) are excluded by design. |

Test coverage: 514 unit tests across the workspace (up from 371 at the
start of the sprint).  `cargo fmt --check`, `cargo clippy
--workspace --all-targets -- -D warnings`, and `cargo test --workspace`
all pass.

## Deliberately deferred

### Ruff parser fork — ✅ landed since this sprint

This follow-up has been resolved. `ruff_python_parser`,
`ruff_python_ast`, `ruff_python_trivia`, `ruff_source_file`, and
`ruff_text_size` are vendored under `tyc/vendor/` with `let`/`mut`
soft-keyword support and a `Mutability` field on assignment AST
nodes. Every consumer crate now parses through
`tyc_syntax::parse_module`; the `rustpython-parser` dependency has
been removed. See `tyc/vendor/README.md` for the migration record.

`ruff_python_codegen` was *not* vendored — `tyc-emit` retains its
hand-written printer because upstream codegen does not expose the
per-statement line-offset hook required for `.py.map` source maps.
Vendoring it remains an open optional follow-up.

### Auto-gather inference and loop parallelisation — ✅ landed since this sprint

Both have shipped behind `[strictness]` opt-ins. Straight-line
independent `await` runs inside an `async def` fold into an
`asyncio.TaskGroup` when `auto-gather = true`; pure list
comprehensions over an iterable that meets `parallel-min-size`
rewrite to a thread-pool map when `auto-parallel = true`. Both
respect the six-condition purity check and only fire when the
candidate region is statically safe.

### PEP 695 inference depth

`bind_typevars_and_substitute` solves the common case (one typevar
per signature, two-way unification through generic containers).
Bounded type vars (`type T = T: Comparable`), variance
considerations, and higher-kinded forms are not handled — the
engine still treats those positions permissively.  Multi-stmt
inference across helper calls also remains an aspiration.

### Source-map line accuracy — ✅ landed since this sprint

The printer now records a per-statement `line_offsets` table while
emitting; `.py.map` v2 stores a `(out_line → ty_line)` mapping that
`tyc trace` reads to rewrite tracebacks at line granularity, even
across multi-line expansions (`with`-chains, `gather:`, `?`
propagation). The format is JS-source-maps-v3-shaped enough for the
LSP's cross-file go-to-definition path to consume it too.

### `class!` raw-class modifier — prototype landed, polish outstanding

**Status: prototype implemented.** A `class!` modifier now suppresses
the automatic `@dataclass` decorator injection at desugar time. The
preprocessor strips the `!`, records the line in
`PreprocessResult::raw_class_lines`, and `desugar_module_with` skips
the decorator (and the `import dataclasses` injection) for any class
whose `TextRange` starts at one of those byte offsets. Round-trips
through `tyc fmt`. See `crates/tyc-syntax/src/preprocess.rs`,
`crates/tyc-desugar/src/lib.rs:973`, and the regression tests
`raw_class_strips_bang_and_records_line`,
`raw_class_round_trips_via_postprocess`, and
`raw_class_skips_dataclass_decorator`.

The motivating problem: subclassing `torch.nn.Module`, `enum.Enum`,
`typing.NamedTuple`, `unittest.TestCase`, or framework declarative
bases (Django, SQLAlchemy) all need a non-trivial `__init__` that
runs *before* field assignment. The auto-injected dataclass
`__init__` never calls `super().__init__()`, so e.g. `nn.Module._parameters`
is never initialised and the first attribute assignment crashes
inside `Module.__setattr__`.

Working example:

```typhon
import torch.nn as nn

class! MyModel(nn.Module):
    layer: nn.Linear
    dropout: float

    def forward(self, x):
        return self.layer(x)
```

The desugar pass now **auto-generates `__init__`** for any `class!`
that has at least one positional base and no hand-written `def
__init__` in the body. The synthesised constructor calls
`super().__init__()` and then assigns every annotated field through
`self`, in source order. Field defaults flow into the parameter
signature. Hand-written `__init__` blocks are preserved verbatim. So
the example above lowers to:

```python
class MyModel(nn.Module):
    layer: nn.Linear
    dropout: float

    def __init__(self, layer: nn.Linear, dropout: float) -> None:
        super().__init__()
        self.layer = layer
        self.dropout = dropout

    def forward(self, x):
        return self.layer(x)
```

Outstanding before this can ship as a documented feature:

- **Language guide.** No mention in `docs/language.md` yet — needs a
  short section explaining when to reach for `class!` vs. `class` /
  `model` / `interface`.
- **Type-checker integration.** A `class!` body still gets the same
  treatment as a plain `class` in `tyc-types`; in particular the
  field-initialisation requirement no longer applies (the user's
  `__init__` does the work), but the checker doesn't know that. Today
  this is fine because field annotations on a raw class compile to
  bare class-level annotations, but a future check that warns on
  missing initialisers needs to be class-kind-aware.
- **`tyc migrate` symmetry.** Going `.py` → `.ty`, classes with an
  explicit `__init__` that doesn't match the dataclass shape should
  emit as `class!` rather than `class`. Currently `migrate` always
  emits plain `class`.
- **LSP hover.** `class!` declarations should advertise themselves in
  hover output the way `mut`/`let` bindings do.
- **Cross-module raw-class tracking.** The byte-offset list is
  per-module today. If a future pass needs to know "is this class
  raw?" from a different module, the resolver should propagate the
  flag onto the binding metadata rather than rely on line lookup.

### Runtime stubtest probe

`tyc check --stubs` performs an AST diff today.  mypy's `stubtest`
proper imports the module at runtime and inspects attributes via
introspection, which catches dynamically-created members the AST
cannot see.  Adding a sandboxed runtime probe is a follow-up; the
AST diff already covers the most common drift sources (rename,
delete, signature change).

## Recommended next steps, in order

> **Update (May 2026 — second pass):** items 3, 5, and 6 below have all
> landed since the last revision. The current list of open follow-ups is:

1. Promote `bind_typevars_and_substitute` into a full structural
   sub-type checker. Bounded type-var checking and basic
   conformance now use the substitution table; variance and
   higher-kinded forms remain partial.
2. Vendor `ruff_python_codegen` to replace `tyc-emit/src/printer.rs`,
   preserving the `line_offsets` hook the hand-written printer
   exposes today (see `tyc/vendor/README.md`).
3. ✅ **Runtime `stubtest` probe** — landed as `tyc stubtest`. The
   command builds the project, walks the output for `.pyi` stubs,
   derives Python import paths, and invokes
   `python -m mypy.stubtest <module>` with `PYTHONPATH` pointing at
   the build directory. Flags mirror `tyc ty` where they overlap;
   `--keep-going` gets the full drift report in CI rather than
   stopping at the first failure. Documented in `docs/cli.md` and
   surfaced in the README's subcommand table.
4. `ty` integration as a complementary second-stage checker (see
   `docs/ty-integration.md`). The subprocess form (`tyc ty`) is
   shipped; the embedded-library form is still future work.
5. ✅ **Richer comptime — `comptime def` functions.** The evaluator
   now dispatches into user-declared `comptime def` helpers from
   any `comptime let` RHS. v1 body grammar covers `return EXPR`,
   local `let`/`mut`/plain assignments, `if`/`elif`/`else`, the
   ternary `EXPR if COND else EXPR`, comparisons, and short-circuit
   `and`/`or`/`not`. Recursion depth is capped at 64. Loops, exceptions,
   `with`-blocks, and `class`/`def` declarations stay rejected.
   Examples in `docs/language.md`. Types as values remain future
   work.
6. ✅ **`class!` polish** — documented in `docs/language.md`,
   surfaced in LSP hover (rendered as "raw class (`class!`)"), and
   `tyc migrate` now promotes a `class Name(...)` declaration to
   `class! Name(...)` whenever the body declares a hand-written
   `def __init__` at the immediate body indent and the class isn't
   already opting into dataclass semantics via `@dataclass`. The
   resolver carries a `ClassKind { Plain, Raw }` on every class
   binding so downstream passes can branch on the marker without
   re-scanning byte ranges; cross-file go-to-definition jumps
   carry the metadata along, so hover after a jump still renders
   the correct kind.
