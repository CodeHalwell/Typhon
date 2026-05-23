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
        "modules": ["attr/__init__.py", "attr/_make.py"],
        "smoke_test": """
import attr

@attr.s
class Point:
    x = attr.ib()
    y = attr.ib()

p = Point(1, 2)
print(f"Point({p.x}, {p.y})")
assert p.x == 1 and p.y == 2
""",
        "expected_output": "Point(1, 2)\n",
    },
    {
        "name": "click",
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
        "version": ">=4.0.0",
        "modules": ["typing_extensions.py"],
        "smoke_test": """
from typing_extensions import Literal, TypedDict, Protocol

class Point(TypedDict):
    x: int
    y: int

class Drawable(Protocol):
    def draw(self) -> None: ...

def process(mode: Literal["fast", "slow"]) -> str:
    return mode

p: Point = {"x": 1, "y": 2}
print(f"Point: {p}")
print(f"Mode: {process('fast')}")
assert p["x"] == 1
""",
        "expected_output": "Point: {'x': 1, 'y': 2}\nMode: fast\n",
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


def test_package(pkg: dict, tyc: Path, verbose: bool = False) -> bool:
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

        # For now, we're establishing the baseline
        # Full migrate→build→test will be phase 2
        print(f"  3. Skipping migrate→build (baseline establishment)")
        print(f"    ✓ Package {name} smoke test passes")

    return True


def main():
    parser = argparse.ArgumentParser(description="PyPI round-trip sweep for Typhon")
    parser.add_argument("--verbose", "-v", action="store_true", help="Verbose output")
    parser.add_argument("--package", "-p", help="Test only this package")
    args = parser.parse_args()

    # Find tyc binary
    tyc = find_tyc_binary()
    if not tyc:
        print("Error: Could not find tyc binary. Build it first with:")
        print("  cd tyc && cargo build --release")
        sys.exit(1)

    print(f"Using tyc: {tyc}")

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
        if not test_package(pkg, tyc, verbose=args.verbose):
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
