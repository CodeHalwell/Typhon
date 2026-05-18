# Risks and Mitigations

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

The risk in Typhon is **not technological**. Every individual stage has a mature, MIT-licensed Rust implementation to depend on, vendor, or learn from. The risk is scope. Two areas will dominate effort: structural type checking and inference, and keeping a forked grammar in sync with upstream Python syntax.

| Risk | Severity | Mitigation |
|------|----------|------------|
| Structural subtyping is months of work | High | Defer to Phase 3. Ship Phase 0–2 without interfaces; nominal types alone are enough to be useful. |
| Ruff parser API drifts under us | Medium | Pin to a SHA; review upstream weekly; keep diff against upstream small. |
| Salsa breaking changes between minor versions | Medium | Every Salsa call sits behind a one-function-wide wrapper module. |
| Free-threaded Python is moving target | Medium | Default off until 3.14 ships as default. Sequential fallback always available via `sys._is_gil_enabled()`. |
| Auto-parallelisation introduces races | High | Ship only the explicit `gather` keyword in v1. Inference comes later, conservative-only. |
| Pydantic coupling alienates users | Medium | Default emit is `@dataclass`, not `BaseModel`. Pydantic is opt-in via `model`. |
| Pre-emptive runtime helpers force a Typhon package on users | Medium | Emit `typhon_runtime/` as generated source the build owns; no PyPI package required. |
| Solo developer burnout on a multi-year project | High | Cut scope aggressively. The minimum-viable Typhon is non-null types + sealed unions + `Result` + dataclass emit. That alone is publishable. |
| Generics syntax choice locks parser fork shape | Resolved | Locked to PEP 695 brackets at Phase 3 entry. The vendored Ruff parser accepts them natively, the resolver and emitter round-trip them unchanged, and divergence from CPython grammar stays at zero. |
| `go` tasks GC'd mid-flight (event loop holds weak refs) | Medium | Lower `go` through `typhon_runtime.tasks.spawn` with a strong-ref registry and done-callback cleanup. Never to a bare `asyncio.create_task`. |
| `asyncio.gather` exception semantics surprise users | Medium | Default `gather:` lowers to `asyncio.TaskGroup` (cancels siblings on first failure). Legacy semantics reserved for explicit `gather(strategy="best-effort"):`. |
| Pydantic's default `extra='ignore'` silently drops input | Medium | `model` emission injects `extra='forbid'` by default. A configurable option for permissive modes is on the roadmap. |
| `.pyi` interop drift from `.dty` source | Medium | `tyc check --stubs` runs an in-tree `stubtest` port against runtime modules; CI gates on `[strictness] stub-check`. |
| Auto-memoisation extends object lifetimes invisibly | Medium | Never silently insert `@functools.cache`. Requires `@memo`, `@pure(memo=True)`, or project-wide `[strictness] auto-memoise = true`, and all six purity conditions must hold. |
| Runtime `isinstance(x, SomeInterface)` gives false confidence | Low | `@runtime_checkable` only validates attribute presence. Typhon refuses to compile `is`/`isinstance` against an interface unless the user opts in via an explicit keyword that lowers to stricter machinery. |
