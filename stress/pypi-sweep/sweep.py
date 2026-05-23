#!/usr/bin/env python3
"""
PyPI sweep harness for Typhon round-trip testing.

For each selected package:
1. pip install into a tempdir venv
2. Find the package's source files
3. Validate baseline: run smoke test on original Python
4. Copy modules to working directory (preserving structure)
5. Run `tyc migrate` on each module
6. Create Typhon project with migrated .ty files
7. Run `tyc build` to emit Python
8. Execute smoke test on emitted code
9. Compare outputs: original vs. emitted

Exit 0 if all packages round-trip cleanly; exit 1 on any failure.
"""

import argparse
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import List, Optional


# Candidate packages selected for light dependencies, good typing, small size
PACKAGES = [
    {
        "name": "attrs",
        "import_name": "attrs",
        "version": ">=23.0.0",
        "modules": ["attrs/__init__.py", "attrs/converters.py"],
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
        "import_name": "click",
        "version": ">=8.0.0",
        "modules": ["click/core.py", "click/decorators.py"],
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
    {
        "name": "typing-extensions",
        "import_name": "typing_extensions",
        "version": ">=4.0.0",
        "modules": ["typing_extensions.py"],
        "smoke_test": """
from typing_extensions import Literal, TypedDict, Protocol

# Test Literal
def process_mode(mode: Literal["read", "write"]) -> str:
    return f"Mode: {mode}"

# Test TypedDict
class Person(TypedDict):
    name: str
    age: int

# Test Protocol
class Drawable(Protocol):
    def draw(self) -> None: ...

# Run smoke test
result = process_mode("read")
print(result)
assert result == "Mode: read"

person: Person = {"name": "Test", "age": 30}
print(f"Person: {person['name']}, {person['age']}")
assert person["name"] == "Test"
""",
        "expected_output": "Mode: read\nPerson: Test, 30\n",
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
    # Check TYC environment variable first
    if "TYC" in os.environ:
        tyc_path = Path(os.environ["TYC"])
        if tyc_path.exists():
            return tyc_path
        print(f"Warning: TYC={os.environ['TYC']} does not exist")

    # Try to find repo root via git
    result = run_command(["git", "rev-parse", "--show-toplevel"])
    if result.returncode == 0:
        repo_root = Path(result.stdout.strip())
        candidates = [
            repo_root / "tyc" / "target" / "release" / "tyc",
            repo_root / "tyc" / "target" / "debug" / "tyc",
        ]
        for candidate in candidates:
            if candidate.exists():
                return candidate

    # Fallback to relative paths from this script
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
        import_name = pkg.get("import_name", pkg["name"].replace("-", "_"))
        result = run_command([
            str(python), "-c",
            f"import {import_name}; import os; print(os.path.dirname({import_name}.__file__))"
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

        # Validate baseline output if expected_output is provided
        expected_output = pkg.get("expected_output")
        if expected_output and original_output != expected_output:
            print(f"    ✗ Baseline output mismatch:")
            print(f"      Expected: {repr(expected_output)}")
            print(f"      Got:      {repr(original_output)}")
            return False

        if baseline_only:
            print(f"  3. Skipping migrate→build (baseline mode)")
            print(f"    ✓ Package {name} baseline passes")
            return True

        # 4. Copy modules to working directory for migration (preserving structure)
        print(f"  3. Copying modules for migration...")
        work_dir = tmppath / "work"
        work_dir.mkdir()

        modules_copied = []
        for module_path in pkg["modules"]:
            src_file = pkg_src / module_path
            if src_file.is_relative_to(pkg_src):
                # Use relative path from pkg_src
                relative_path = src_file.relative_to(pkg_src)
            else:
                # src_file might be absolute path starting at pkg_src parent
                relative_path = Path(module_path)

            if not src_file.exists():
                print(f"    ⚠ Module not found: {module_path} (skipping)")
                continue

            # Preserve directory structure
            dest_file = work_dir / relative_path
            dest_file.parent.mkdir(parents=True, exist_ok=True)
            dest_file.write_text(src_file.read_text())
            modules_copied.append((relative_path, dest_file))

        if not modules_copied:
            print(f"    ✗ No modules found to migrate")
            return False

        print(f"    ✓ Copied {len(modules_copied)} module(s)")

        # 5. Run tyc migrate on each module
        print(f"  4. Running tyc migrate...")
        migrated_files = []
        for relative_path, py_file in modules_copied:
            result = run_command([str(tyc), "migrate", str(py_file)], cwd=work_dir)
            if result.returncode != 0:
                print(f"    ✗ Migration failed for {relative_path}:")
                if verbose:
                    print(f"      {result.stderr}")
                return False

            ty_file = py_file.with_suffix(".ty")
            if ty_file.exists():
                migrated_files.append((relative_path.with_suffix(".ty"), ty_file))

        if not migrated_files:
            print(f"    ✗ No .ty files produced by migration")
            return False

        print(f"    ✓ Migrated {len(migrated_files)} file(s)")

        # 6. Create a Typhon project around the migrated files (preserving structure)
        print(f"  5. Creating Typhon project...")
        project_dir = tmppath / "typhon_project"
        project_dir.mkdir()
        src_dir = project_dir / "src"
        src_dir.mkdir()

        # Copy migrated files to project src/, preserving directory structure
        for relative_path, ty_file in migrated_files:
            dest_path = src_dir / relative_path
            dest_path.parent.mkdir(parents=True, exist_ok=True)
            dest_path.write_text(ty_file.read_text())

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

        # 8. Execute emitted code and compare outputs
        print(f"  7. Running smoke test on emitted Python...")
        build_dir = project_dir / "build"

        # Create test file that imports from build directory
        emitted_test = tmppath / "test_emitted.py"
        emitted_test_code = f"""
import sys
sys.path.insert(0, '{build_dir}')
{pkg["smoke_test"]}
"""
        emitted_test.write_text(emitted_test_code)

        result = run_command([str(python), str(emitted_test)])
        if result.returncode != 0:
            print(f"    ✗ Emitted smoke test failed:")
            if verbose:
                print(f"      {result.stderr}")
            return False

        emitted_output = result.stdout
        print(f"    ✓ Emitted test passed")
        if verbose:
            print(f"      Output: {emitted_output.strip()}")

        # 9. Compare outputs
        print(f"  8. Comparing outputs...")
        if emitted_output != original_output:
            print(f"    ✗ Semantic drift detected:")
            print(f"      Original: {repr(original_output)}")
            print(f"      Emitted:  {repr(emitted_output)}")
            return False

        print(f"    ✓ Outputs match")
        print(f"    ✓ Package {name} round-trip passes")

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
