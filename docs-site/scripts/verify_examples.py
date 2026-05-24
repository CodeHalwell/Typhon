#!/usr/bin/env python3
"""Verify that complete Typhon examples in docs site MDX files compile.

This is the audit harness used during the docs-examples-audit pass. It
walks every `.mdx` under `src/content/docs/`, extracts ```python /
```typhon / ```ty code blocks, classifies each one, and tries to
compile the complete-program blocks via `tyc build` in a temp project.

Usage:
    # Build tyc first:
    cd ../tyc && cargo build --release && cd -

    # Run the audit (writes a JSON report to stdout, progress to stderr):
    python3 scripts/verify_examples.py > audit.json

    # Filter just the real issues (skipping partial-snippet noise):
    python3 scripts/verify_examples.py --real-only

What counts as a real issue:
    A complete-program block that fails `tyc build` with a diagnostic
    other than `tyc::unknown_name`, `tyc::main_not_called`,
    `tyc::missing_argument`, `tyc::missing_annotation`, or
    `tyc::unsafe_unused_block` (the diagnostics typically fired by
    partial snippets that reference names from earlier in the page).

Exit code: 0 if no real issues, 1 otherwise.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

DOCS_ROOT = Path(__file__).resolve().parents[1] / "src" / "content" / "docs"
TYC_CANDIDATES = [
    Path(__file__).resolve().parents[2] / "tyc" / "target" / "release" / "tyc",
    Path(__file__).resolve().parents[2] / "tyc" / "target" / "debug" / "tyc",
]
TIMEOUT_SECS = 30

DEFAULT_TYPHON_TOML = """\
[project]
name = "test"
version = "0.1.0"
src = "src"
out = "build"

[python]
target = "3.13"

[emit]
class-default = "dataclass"
format = false

[strictness]
no-implicit-any = true
unused-import = "warn"
exhaustive-match = "error"
"""

PARTIAL_SNIPPET_CODES = {
    "tyc::unknown_name",
    "tyc::main_not_called",
    "tyc::missing_argument",
    "tyc::missing_annotation",
    "tyc::unsafe_unused_block",
}


def find_tyc() -> str:
    for c in TYC_CANDIDATES:
        if c.exists():
            return str(c)
    raise SystemExit(
        "tyc binary not found. Build it first:\n"
        "  cd tyc && cargo build --release"
    )


def extract_blocks(text: str):
    """Yield (lang, body, line_no) for every fenced code block.

    Handles arbitrarily-indented fences so blocks nested inside
    starlight `<Steps>` / `<TabItem>` (with 2- or 4-space indents)
    are surfaced too. The returned body is de-indented to column 0.
    """
    out: list[tuple[str, str, int]] = []
    lines = text.split("\n")
    i = 0
    while i < len(lines):
        line = lines[i]
        stripped = line.lstrip()
        leading = len(line) - len(stripped)
        if stripped.startswith("```") and stripped != "```":
            lang = stripped[3:].strip()
            j = i + 1
            body_lines: list[str] = []
            while j < len(lines):
                l2 = lines[j]
                if (
                    l2.lstrip().startswith("```")
                    and l2.lstrip() == "```"
                    and (len(l2) - len(l2.lstrip())) == leading
                ):
                    break
                if l2.startswith(" " * leading):
                    body_lines.append(l2[leading:])
                else:
                    body_lines.append(l2)
                j += 1
            out.append((lang, "\n".join(body_lines), i + 1))
            i = j + 1
        else:
            i += 1
    return out


def is_intentional_error(body: str) -> bool:
    """Heuristic: examples marked with `❌` are intended to fail."""
    if "❌" in body:
        return True
    if "tyc::" in body and (
        "error[" in body.lower()
        or "warning[" in body.lower()
        or "advice[" in body.lower()
    ):
        return True
    return False


def is_partial(body: str) -> bool:
    """Heuristic: the block has no top-level declaration."""
    lines = [l for l in body.split("\n") if l.strip()]
    if not lines:
        return True
    top_keywords = (
        "def ", "class ", "let ", "mut ", "import ", "from ", "type ",
        "interface ", "model ", "@", "extend ", "impl ", "newtype ",
        "pub ", "freeze ", "lazy ", "comptime ", "if __name__",
        "plain class", "class!",
    )
    for l in lines:
        if l.startswith(" ") or l.startswith("\t"):
            continue
        if any(l.startswith(kw) for kw in top_keywords):
            return False
    return True


def looks_like_emitted_python(body: str) -> bool:
    """Heuristic: this block is showing emitted Python, not Typhon source."""
    has_typhon_kw = bool(
        re.search(
            r"\b(let|mut|impl|extend|model|interface|guard|gather|"
            r"comptime|newtype|freeze|pub)\b",
            body,
        )
    )
    has_emit_markers = bool(
        re.search(r"@dataclass|typhon_runtime|BaseModel|__typhon_", body)
    )
    return has_emit_markers and not has_typhon_kw


def compile_typhon(tyc: str, body: str) -> dict:
    tmp = tempfile.mkdtemp(prefix="tyc_audit_")
    try:
        (Path(tmp) / "typhon.toml").write_text(DEFAULT_TYPHON_TOML)
        src = Path(tmp) / "src"
        src.mkdir()
        (src / "main.ty").write_text(body)
        proc = subprocess.run(
            [tyc, "build"],
            cwd=tmp,
            capture_output=True,
            text=True,
            timeout=TIMEOUT_SECS,
        )
        return {
            "build_ok": proc.returncode == 0,
            "build_output": proc.stdout + "\n" + proc.stderr,
        }
    except subprocess.TimeoutExpired:
        return {"build_ok": False, "build_output": "TIMEOUT"}
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def process_file(tyc: str, path: Path):
    rel = path.relative_to(DOCS_ROOT)
    text = path.read_text()
    results: list[dict] = []
    for lang, body, line_no in extract_blocks(text):
        if lang not in ("python", "typhon", "ty"):
            continue
        if looks_like_emitted_python(body):
            continue
        if is_intentional_error(body):
            results.append({"line": line_no, "kind": "intentional_error", "ok": True})
            continue
        if is_partial(body):
            results.append({"line": line_no, "kind": "partial", "ok": True})
            continue
        try:
            r = compile_typhon(tyc, body)
            results.append({
                "line": line_no,
                "kind": "complete",
                "ok": r["build_ok"],
                "output": r["build_output"][-1500:] if not r["build_ok"] else "",
            })
        except Exception as e:
            results.append({"line": line_no, "kind": "exception", "ok": False, "output": str(e)})
    return str(rel), results


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--real-only",
        action="store_true",
        help="Only print issues that aren't likely partial-snippet noise.",
    )
    parser.add_argument(
        "--workers",
        type=int,
        default=4,
        help="Parallel `tyc build` workers (default 4).",
    )
    args = parser.parse_args()

    tyc = find_tyc()
    files = sorted(DOCS_ROOT.rglob("*.mdx"))
    print(f"Processing {len(files)} files (workers={args.workers})...", file=sys.stderr)

    all_results: dict[str, list[dict]] = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as ex:
        futures = {ex.submit(process_file, tyc, f): f for f in files}
        for i, fut in enumerate(concurrent.futures.as_completed(futures), 1):
            rel, results = fut.result()
            all_results[rel] = results
            fails = [r for r in results if not r["ok"]]
            tag = f"{len(fails)} FAIL" if fails else "ok"
            print(f"[{i}/{len(files)}] {rel}: {tag}", file=sys.stderr)

    # Summarise
    total_complete = sum(
        1 for rel, res in all_results.items() for r in res if r["kind"] == "complete"
    )
    real_fails = 0
    partial_fails = 0
    for rel, results in all_results.items():
        for r in results:
            if r["ok"]:
                continue
            codes = set(re.findall(r"tyc::\w+", r.get("output", "")))
            if codes and codes <= PARTIAL_SNIPPET_CODES:
                partial_fails += 1
            else:
                real_fails += 1

    print(f"\n=== SUMMARY ===", file=sys.stderr)
    print(f"Complete-program blocks tested: {total_complete}", file=sys.stderr)
    print(f"Real issues:                    {real_fails}", file=sys.stderr)
    print(f"Partial-snippet noise:          {partial_fails}", file=sys.stderr)

    if args.real_only:
        filtered: dict[str, list[dict]] = {}
        for rel, results in all_results.items():
            keep = []
            for r in results:
                if r["ok"]:
                    continue
                codes = set(re.findall(r"tyc::\w+", r.get("output", "")))
                if codes and codes <= PARTIAL_SNIPPET_CODES:
                    continue
                keep.append(r)
            if keep:
                filtered[rel] = keep
        json.dump(filtered, sys.stdout, indent=2)
    else:
        json.dump(all_results, sys.stdout, indent=2)
    sys.stdout.write("\n")

    return 1 if real_fails > 0 else 0


if __name__ == "__main__":
    sys.exit(main())
