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
