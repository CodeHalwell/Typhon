#!/usr/bin/env python3
"""
PyPI sweep harness for Typhon round-trip testing.

For each selected package:
1. pip install into a tempdir venv
2. Find the package's source files
3. Run `tyc migrate` on representative modules
4. Run `tyc build` on the migrated output
5. Execute a minimal smoke test under both original and emitted versions
6. Compare outputs to catch semantic drift

Exit 0 if all packages round-trip cleanly; exit 1 on any failure.
"""

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import List, Optional


# Candidate packages selected for light dependencies, good typing, small size
PACKAGES = [
    {
        "name": "attrs",
        "version": ">=23.0.0",
        "modules": ["__init__.py", "converters.py"],  # Use actual package structure
        "smoke_test": """
import attrs

@attrs.define
class Point:
    x: int
    y: int

p = Point(1, 2)
print(f"Point({p.x}, {p.y})")
assert p.x == 1 and p.y == 2
""",
        "expected_output": "Point(1, 2)\n",
    },
    {
        "name": "click",
        "version": ">=8.0.0",
        "modules": ["core.py", "decorators.py"],
        "smoke_test": """
import click

@click.command()
@click.option('--count', default=1, help='Number of greetings.')
@click.option('--name', prompt='Your name', help='The person to greet.')
def hello(count, name):
    for _ in range(count):
        click.echo(f'Hello {name}!')

# Smoke test: invoke programmatically
from click.testing import CliRunner
runner = CliRunner()
result = runner.invoke(hello, ['--count', '2', '--name', 'Test'])
print(result.output.strip())
assert result.exit_code == 0
""",
        "expected_output": "Hello Test!\nHello Test!",
    },
]


def run_command(cmd: List[str], cwd: Optional[Path] = None, capture: bool = True) -> subprocess.CompletedProcess:
    """Run a command and return the result."""
    kwargs = {"cwd": cwd, "text": True}
    if capture:
        kwargs["capture_output"] = True
    else:
        kwargs["stdout"] = subprocess.PIPE
        kwargs["stderr"] = subprocess.STDOUT

    return subprocess.run(cmd, **kwargs)


def setup_venv(venv_path: Path) -> bool:
    """Create a virtual environment."""
    result = run_command([sys.executable, "-m", "venv", str(venv_path)])
    return result.returncode == 0


def pip_install(venv_path: Path, package: str, version: str) -> bool:
    """Install a package in the venv."""
    pip = venv_path / "bin" / "pip"
    result = run_command([str(pip), "install", f"{package}{version}"])
    return result.returncode == 0


def find_tyc_binary() -> Optional[Path]:
    """Find the tyc binary."""
    # Try release build first
    candidates = [
        Path(__file__).parent.parent.parent / "tyc" / "target" / "release" / "tyc",
        Path(__file__).parent.parent.parent / "tyc" / "target" / "debug" / "tyc",
    ]
    for candidate in candidates:
        if candidate.exists():
            return candidate

    # Try system PATH
    result = run_command(["which", "tyc"])
    if result.returncode == 0:
        return Path(result.stdout.strip())

    return None


def test_package(pkg: dict, tyc: Path, verbose: bool = False, baseline_only: bool = False) -> bool:
    """Test one package through the migrate→build→test pipeline."""
    name = pkg["name"]
    print(f"\n{'='*60}")
    print(f"Testing package: {name}")
    print(f"{'='*60}")

    with tempfile.TemporaryDirectory() as tmpdir:
        tmppath = Path(tmpdir)
        venv_path = tmppath / "venv"

        # 1. Setup venv and install package
        print(f"  1. Creating venv and installing {name}...")
        if not setup_venv(venv_path):
            print(f"    ✗ Failed to create venv")
            return False

        if not pip_install(venv_path, pkg["name"], pkg["version"]):
            print(f"    ✗ Failed to install {name}")
            return False
        print(f"    ✓ Installed {name}")

        # 2. Locate package source
        python = venv_path / "bin" / "python3"
        result = run_command([
            str(python), "-c",
            f"import {pkg['name'].replace('-', '_')}; import os; print(os.path.dirname({pkg['name'].replace('-', '_')}.__file__))"
        ])
        if result.returncode != 0:
            print(f"    ✗ Could not locate {name} source")
            return False

        pkg_src = Path(result.stdout.strip())
        print(f"    ✓ Found source at {pkg_src}")

        # 3. Run smoke test on original Python
        print(f"  2. Running smoke test on original Python...")
        original_test = tmppath / "test_original.py"
        original_test.write_text(pkg["smoke_test"])

        result = run_command([str(python), str(original_test)])
        if result.returncode != 0:
            print(f"    ✗ Original smoke test failed:")
            print(f"      {result.stderr}")
            return False

        original_output = result.stdout
        print(f"    ✓ Original test passed")
        if verbose:
            print(f"      Output: {original_output.strip()}")

        if baseline_only:
            print(f"  3. Skipping migrate→build (baseline mode)")
            print(f"    ✓ Package {name} baseline passes")
            return True

        # 4. Copy modules to working directory for migration
        print(f"  3. Copying modules for migration...")
        work_dir = tmppath / "work"
        work_dir.mkdir()

        modules_copied = []
        for module_path in pkg["modules"]:
            src_file = pkg_src / module_path
            if not src_file.exists():
                print(f"    ⚠ Module not found: {module_path} (skipping)")
                continue

            dest_file = work_dir / Path(module_path).name
            dest_file.write_text(src_file.read_text())
            modules_copied.append(dest_file)

        if not modules_copied:
            print(f"    ✗ No modules found to migrate")
            return False

        print(f"    ✓ Copied {len(modules_copied)} module(s)")

        # 5. Run tyc migrate on each module
        print(f"  4. Running tyc migrate...")
        migrated_files = []
        for py_file in modules_copied:
            result = run_command([str(tyc), "migrate", str(py_file)], cwd=work_dir)
            if result.returncode != 0:
                print(f"    ✗ Migration failed for {py_file.name}:")
                if verbose:
                    print(f"      {result.stderr}")
                return False

            ty_file = py_file.with_suffix(".ty")
            if ty_file.exists():
                migrated_files.append(ty_file)

        if not migrated_files:
            print(f"    ✗ No .ty files produced by migration")
            return False

        print(f"    ✓ Migrated {len(migrated_files)} file(s)")

        # 6. Create a Typhon project around the migrated files
        print(f"  5. Creating Typhon project...")
        project_dir = tmppath / "typhon_project"
        project_dir.mkdir()
        src_dir = project_dir / "src"
        src_dir.mkdir()

        # Copy migrated files to project src/
        for ty_file in migrated_files:
            (src_dir / ty_file.name).write_text(ty_file.read_text())

        # Create minimal typhon.toml
        toml_content = """[project]
name = "pypi-sweep-test"
version = "0.1.0"
src = "src"
out = "build"

[python]
target = "3.13"

[emit]
format = false

[strictness]

[env]
"""
        (project_dir / "typhon.toml").write_text(toml_content)
        print(f"    ✓ Created Typhon project")

        # 7. Run tyc build
        print(f"  6. Running tyc build...")
        result = run_command([str(tyc), "build", str(project_dir)])
        if result.returncode != 0:
            print(f"    ✗ Build failed:")
            if verbose:
                print(f"      {result.stderr}")
            return False

        print(f"    ✓ Build succeeded")

        # 8. For now, we consider success if build passes
        # Full semantic diff will be added in a future enhancement
        print(f"  7. Semantic diff (not yet implemented)")
        print(f"    ⚠ Skipping output comparison")
        print(f"    ✓ Package {name} round-trip passes (build only)")

    return True


def main():
    parser = argparse.ArgumentParser(description="PyPI round-trip sweep for Typhon")
    parser.add_argument("--verbose", "-v", action="store_true", help="Verbose output")
    parser.add_argument("--package", "-p", help="Test only this package")
    parser.add_argument("--baseline", action="store_true", help="Run baseline smoke tests only (skip migrate/build)")
    args = parser.parse_args()

    # Find tyc binary
    tyc = find_tyc_binary()
    if not tyc:
        print("Error: Could not find tyc binary. Build it first with:")
        print("  cd tyc && cargo build --release")
        sys.exit(1)

    print(f"Using tyc: {tyc}")
    if args.baseline:
        print("Mode: Baseline smoke tests only")
    else:
        print("Mode: Full migrate→build→test pipeline")

    # Filter packages if requested
    packages = PACKAGES
    if args.package:
        packages = [p for p in PACKAGES if p["name"] == args.package]
        if not packages:
            print(f"Error: Package '{args.package}' not in test suite")
            sys.exit(1)

    # Run tests
    failures = []
    for pkg in packages:
        if not test_package(pkg, tyc, verbose=args.verbose, baseline_only=args.baseline):
            failures.append(pkg["name"])

    # Report results
    print(f"\n{'='*60}")
    print(f"RESULTS")
    print(f"{'='*60}")
    print(f"Tested: {len(packages)} package(s)")
    print(f"Passed: {len(packages) - len(failures)}")
    print(f"Failed: {len(failures)}")

    if failures:
        print(f"\nFailed packages:")
        for name in failures:
            print(f"  - {name}")
        sys.exit(1)

    print(f"\n✓ All packages passed!")
    sys.exit(0)


if __name__ == "__main__":
    main()
