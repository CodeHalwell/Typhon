# Epic: Corpus Coverage — Third-Party PyPI Sweep + Drift Round 4 + Inter-Procedural Audit

**Issue:** CodeHalwell/Typhon#<issue-number>
**Status:** Partially complete (2 of 3 sub-items done)

## Overview

This epic tracks three independent initiatives to improve Typhon's test coverage and audit capabilities beyond what current CI exercises.

## Sub-Item 1: Third-Party PyPI Project Round-Trip Sweep ✅

**Goal:** Test real PyPI packages through `tyc migrate` + `tyc build` + semantic diff to catch regressions invisible in hand-written fixtures.

**Deliverables:**
- ✅ PyPI sweep harness (`stress/pypi-sweep/sweep.py`)
- ✅ Package selection criteria documented
- ✅ Phase 1: Baseline smoke tests (packages install and run correctly)
- ✅ Phase 2: Full migrate→build→semantic diff pipeline
  - ✅ Preserves module structure during migration
  - ✅ Validates baseline outputs
  - ✅ Executes emitted code
  - ✅ Compares original vs. emitted outputs
- ✅ Findings documented
- ✅ Environment variable support (TYC override)
- ✅ Git-based repo detection

**Key Findings:**
- Migration infrastructure works end-to-end with semantic diff
- Real PyPI packages (attrs, typing-extensions) expose edge cases:
  - attrs: Too complex (28 type errors post-migration)
  - typing-extensions: Migration bug (`mut else:` invalid syntax)
- Package selection is critical — need simpler, well-typed utilities

**Future Work:**
- Refine package selection (try `python-dateutil`, `humanize`)
- Wire into CI as opt-in nightly job
- Test remaining packages (click Phase 2)

**Location:** `stress/pypi-sweep/`

---

## Sub-Item 2: `tyc::python_semantic_drift` Audit Round 4 ✅

**Goal:** Probe edge-case Python idioms to detect over-rejections by the type checker.

**Deliverables:**
- ✅ New audit round: `stress/round-2026-05-23-drift-round-4/`
- ✅ 17 probes covering:
  - Walrus operator (comprehensions, conditionals, while loops)
  - Augmented assignment narrowing
  - `yield from` generator delegation
  - Match patterns with `*` capture
  - String operations
  - `raise X from Y`
  - `*args` / `**kwargs` unpacking

**Result:** **17/17 probes pass** — no over-rejections found.

All tested idioms are correctly accepted by Typhon's type checker. No `tyc::python_semantic_drift` diagnostics filed.

**Location:** `stress/round-2026-05-23-drift-round-4/`

---

## Sub-Item 3: General Inter-Procedural Field-Init Audit ⏳

**Goal:** Extend the field-init audit beyond the trivial `return X.__new__(X)` pattern to track:
- Helpers that partially initialize instances
- Multi-step factory chains
- Parameters that are partial instances

**Current State (PR #105):**
- Narrow pattern: `def make(): return X.__new__(X)`
- Call sites register LHS as tracked
- Escape checks fire `tyc::missing_field_init`

**Proposed Enhancement:**
- Design summary IR per function (tracks param field assigns, return partial status)
- Compute summaries in analysis pass
- Consume at call sites to update tracked partial instances
- Support composition: `make_partial() → init_basic() → finish_config()`

**Status:** Design documented in `stress/interprocedural-audit-design.md`

**Future Work:**
- Implement `FunctionSummary` data structure
- Add `compute_function_summary` analysis pass
- Update call-site tracking to consume summaries
- Add comprehensive tests for inter-procedural patterns
- Document limitations in roadmap

**Location:** `stress/interprocedural-audit-design.md` (design only)

---

## Summary

| Sub-Item | Status | Output |
|----------|--------|--------|
| 1. PyPI Sweep | ✅ Complete | Infrastructure + findings |
| 2. Drift Round 4 | ✅ Complete | 17/17 probes pass |
| 3. Inter-Procedural Audit | 📝 Designed | Implementation pending |

**Overall Epic Status:** 2/3 sub-items completed. Sub-item 3 requires significant refactoring (larger change) and is documented for future implementation.
