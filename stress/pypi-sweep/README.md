# PyPI Round-Trip Sweep

Tests real third-party Python packages through the Typhon migrate→build→test pipeline to catch regressions in the migrator and emitter that aren't visible in hand-written test fixtures.

## Current State

**Phase 1 (Baseline):** Establishes that selected packages install and run correctly in vanilla Python. The `sweep.py` script:
- Creates isolated venvs for each package
- Installs the package via pip
- Runs a minimal smoke test to verify baseline functionality
- Validates output against expected values

**Phase 2 (Implemented):** Full round-trip testing through `tyc migrate` + `tyc build` + semantic diff:
- Preserves module structure during migration
- Builds emitted Python from migrated Typhon code
- Executes smoke tests on both original and emitted versions
- Compares outputs to detect semantic drift

## Package Selection

Packages chosen for:
- **Good typing coverage** (typed stubs or inline annotations)
- **Light dependency footprint** (fast installs, no C extensions if possible)
- **Small surface area** (testing migration quality, not framework complexity)

### Current Test Suite

1. **attrs** (>=23.0.0)
   - Decorator-based class construction (`@attrs.define`)
   - Type-annotated attributes
   - Smoke test: Create and validate a simple `@attrs.define` class

2. **click** (>=8.0.0)
   - Command-line argument parsing with decorators
   - Type annotations on parameters
   - Smoke test: Programmatic CLI invocation with `CliRunner`

3. **typing-extensions** (>=4.0.0)
   - Backport of newer typing features (`Literal`, `TypedDict`, `Protocol`)
   - Pure-Python, no runtime logic beyond type definitions
   - Smoke test: Use `Literal`, `TypedDict`, `Protocol` in simple code

## Usage

### Run all packages

```bash
python3 stress/pypi-sweep/sweep.py
```

### Run a specific package

```bash
python3 stress/pypi-sweep/sweep.py --package attrs
```

### Verbose output

```bash
python3 stress/pypi-sweep/sweep.py --verbose
```

## Phase 2 Implementation Notes

The harness now implements the full round-trip pipeline:
1. Validates baseline output against expected values
2. Preserves package directory structure during migration (e.g., `attrs/__init__.py` → `src/attrs/__init__.ty`)
3. Runs `tyc migrate` on each module
4. Creates Typhon project with proper structure
5. Runs `tyc build` to emit Python
6. Executes smoke test on emitted code
7. Compares outputs to detect semantic drift

**Environment Variable Support:**
- Set `TYC=/path/to/tyc` to override the compiler path
- Harness uses `git rev-parse --show-toplevel` to find repo root (consistent with other stress scripts)

## CI Integration (Future)

Wire this into GitHub Actions as an **opt-in nightly job** (not per-PR):
- Full PyPI installs are too heavy for per-commit CI
- Nightly cadence catches regressions without blocking development
- Manual trigger for pre-release validation

Example workflow snippet:
```yaml
name: PyPI Sweep (Nightly)
on:
  schedule:
    - cron: '0 2 * * *'  # 2 AM UTC daily
  workflow_dispatch:     # Manual trigger

jobs:
  pypi-sweep:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Build tyc
        run: cd tyc && cargo build --release
      - name: Run PyPI sweep
        run: python3 stress/pypi-sweep/sweep.py --verbose
```

## Adding New Packages

Edit `PACKAGES` in `sweep.py`:

```python
PACKAGES = [
    # ... existing packages ...
    {
        "name": "your-package",
        "import_name": "your_package",  # Explicit import name (not derived from package name)
        "version": ">=1.0.0",
        "modules": ["your_package/__init__.py"],  # Paths relative to package root (preserves structure)
        "smoke_test": """
# Python code that exercises the package
import your_package
result = your_package.do_something()
print(result)
assert result == expected
""",
        "expected_output": "expected output\n",  # Optional: validates baseline and emitted outputs
    },
]
```

Criteria for new packages:
- Available on PyPI
- Typed (has `py.typed` marker or inline annotations)
- Small (< 5000 LOC for the modules under test)
- Light dependencies (installs quickly, ideally pure-Python)
- Representative of common PyPI patterns (dataclasses, decorators, protocols, etc.)

## Findings

### Phase 1 Results (Baseline)

All selected packages install and run correctly in vanilla Python:
- ✓ attrs (23.x) - smoke test passes
- ✓ click (8.x) - smoke test passes
- ✓ typing-extensions (4.x) - smoke test passes

### Phase 2 Observed Results (Migrate→Build→Semantic Diff)

**Summary:** Migration infrastructure works end-to-end with semantic diff validation. Real PyPI packages expose edge cases in `tyc migrate` and complex typing patterns that require manual fixes.

**attrs (23.x):**
- ✗ Migration produces `.ty` files but build fails with 28 type errors
- Issues:
  - Unused imports (filters, setters, validators) - low severity, could be auto-fixed
  - Complex internal APIs with dynamic patterns
  - Recommendation: Too complex for automated sweep; requires manual migration

**click (8.x):**
- Status: Pending full Phase 2 testing

**typing-extensions (4.x):**
- Migration bug found: `tyc migrate` generates invalid syntax `mut else:` (should be `else:`)
- Location: Line 181 of migrated `typing_extensions.ty`
- This indicates a parser or migration issue when handling non-mut branches in conditional chains

### Key Learnings

1. **Package selection is critical**: Real-world packages like attrs use patterns (metaclasses, runtime introspection, complex generics) that are challenging for automated migration.

2. **Better candidate profile**: Need packages that are:
   - Primarily type-annotated (not @dataclass-heavy or dynamic)
   - Simple module structure (few imports, limited cross-module dependencies)
   - Representative but not framework-level complexity

3. **Suggested next packages to try**:
   - `python-dateutil` (date/time utilities, well-typed)
   - `humanize` (simple formatting utilities)
   - Small utility packages with <1000 LOC

### Known Migration Bugs

**typing-extensions**: `tyc migrate` generates invalid syntax `mut else:` (should be `else:`). This indicates a parser or migration issue when handling non-mut branches in conditional chains.

Location: Line 181 of migrated `typing_extensions.ty`

