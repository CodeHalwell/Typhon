# PyPI Round-Trip Sweep

Tests real third-party Python packages through the Typhon migrate→build→test pipeline to catch regressions in the migrator and emitter that aren't visible in hand-written test fixtures.

## Current State

**Phase 1 (Baseline):** Establishes that selected packages install and run correctly in vanilla Python. The `sweep.py` script:
- Creates isolated venvs for each package
- Installs the package via pip
- Runs a minimal smoke test to verify baseline functionality

**Phase 2 (Planned):** Full round-trip testing through `tyc migrate` + `tyc build` + semantic diff.

## Package Selection

Packages chosen for:
- **Good typing coverage** (typed stubs or inline annotations)
- **Light dependency footprint** (fast installs, no C extensions if possible)
- **Small surface area** (testing migration quality, not framework complexity)

### Current Test Suite

1. **attrs** (>=23.0.0)
   - Decorator-based class construction (`@attr.s`)
   - Type-annotated attributes
   - Smoke test: Create and validate a simple `@attr.s` class

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

## Phase 2 Roadmap

Enhance `sweep.py` to:
1. Copy selected `.py` files from the installed package to a working directory
2. Run `tyc migrate` on each file
3. Create a Typhon project around the migrated `.ty` files
4. Run `tyc build` to emit Python
5. Run the smoke test against the emitted code
6. Diff the outputs: original vs. migrated+emitted

Exit codes:
- **0**: All packages round-trip cleanly
- **1**: At least one package failed migration, build, or semantic diff

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
        "version": ">=1.0.0",
        "modules": ["your_package/__init__.py"],  # Files to migrate
        "smoke_test": """
# Python code that exercises the package
import your_package
result = your_package.do_something()
print(result)
assert result == expected
""",
        "expected_output": "expected output\n",
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

None yet — Phase 2 (full round-trip) is not implemented.

Once Phase 2 lands, findings will be catalogued here:
- Migration failures (patterns `tyc migrate` can't handle)
- Build failures (type-check errors post-migration)
- Semantic drift (different output between original and emitted code)
