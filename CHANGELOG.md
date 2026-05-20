# Changelog

All notable changes to Typhon are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) loosely; the
canonical phase-by-phase status lives in `docs/roadmap.md`.

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
