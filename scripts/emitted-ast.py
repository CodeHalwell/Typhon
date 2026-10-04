#!/usr/bin/env python3
"""emitted-ast.py — compare the Python ASTs `tyc build` emits for the corpus.

Two tools share one engine here:

  equiv A B   The emitted-AST equivalence harness (step 0 of
              docs/design/sugar-as-ast-nodes.md). A and B are each a `tyc`
              binary or a git revision. Every corpus unit is built with both
              compilers and every emitted `.py` is compared by `ast.dump`; the
              report lists the units whose emitted AST changed. Use it to show
              that a refactor of a lowering is behaviour-preserving, or to see
              exactly which programs a deliberate lowering change touches.

  fmt-gate    The de-formatted-corpus `tyc fmt` gate (CI job `fmt-corpus`,
              requested by W7-03). The corpus is copied, its code is
              de-formatted (`let x: T = 1` -> `let x:T=1`, no space after
              commas or colons, `->` and `=` squeezed, trailing blanks added;
              strings and comments untouched), `tyc fmt` is run over the copy,
              and every unit must then emit exactly the Python AST the
              original corpus emits. `tyc fmt` already refuses to write output
              whose lowered AST differs from its input (tyc_syntax::ast_equiv);
              this gate checks the same property end to end, through `tyc
              build`, over the whole corpus. A de-formatted file `tyc fmt`
              refuses fails the gate too, unless `tyc fmt --check` already
              refuses the file as written (a parse-error probe in stress/).

and two building blocks:

  dump        Build every unit with one binary and record the AST of each
              emitted `.py` (content-addressed, so the shared
              `typhon_runtime/` files cost one blob).
  compare     Compare two dumps.

Units are discovered as scripts/vm-differential.sh does: a directory holding a
`typhon.toml` is one project unit; every other `.ty` file is a standalone unit,
built inside a synthesised minimal project. Builds are network-free
(TYC_NO_SYNC=1, TYC_NO_INTROSPECT=1, `--no-sync`). The corpus is always read
from the working tree (or `--corpus-root`), never from the revisions being
compared, so only the compiler differs between the two sides.

Before comparison each emitted file is normalised the way `tyc fmt`'s own check
is (tyc-syntax/src/ast_equiv.rs): identifiers starting `__typhon_` are renamed
by first appearance (some lowerings number temporaries by source line), and
source positions and comments are ignored. `--lenient-docstrings` compares
docstrings with each line trimmed and blank lines dropped, for when `ruff
format` may have re-indented them; `fmt-gate` always sets it.

Usage:
  python3.13 scripts/emitted-ast.py equiv A B [options]
  python3.13 scripts/emitted-ast.py fmt-gate [--tyc BIN] [options]
  python3.13 scripts/emitted-ast.py dump --tyc BIN --out DIR [options]
  python3.13 scripts/emitted-ast.py compare DIR_A DIR_B

Common options:
  --scope examples|stress|valid|all   corpus subset (default: all, which is
                                      examples + stress + corpus/valid)
  --filter REGEX                      only units whose id matches REGEX
  --jobs N                            parallel builds (default: CPU count)
  --timeout SECONDS                   per-build timeout (default: 60)
  --keep                              keep the scratch directory
  --report PATH                       write the per-unit verdicts as TSV

`equiv` also takes:
  --cargo-target-dir DIR  where revisions are built (default:
                          tyc/target/emitted-ast). Revisions are exported with
                          `git archive` into the scratch directory and built
                          with `cargo build --release --bin tyc`; the working
                          tree and the repository's refs are never touched.
  --lenient-docstrings    see above.

Examples:
  # Did my lowering refactor change any emitted program?
  python3.13 scripts/emitted-ast.py equiv origin/main HEAD
  # Two binaries you already built:
  python3.13 scripts/emitted-ast.py equiv /tmp/tyc-old tyc/target/release/tyc
  # The CI fmt gate, locally:
  python3.13 scripts/emitted-ast.py fmt-gate --tyc tyc/target/release/tyc

Exit status: 0 when the two sides agree on every unit, 1 when any unit
differs, 2 on a usage or environment error. Requires CPython 3.13+ (the
emitted code uses 3.13 syntax, which an older `ast` cannot parse).
"""

from __future__ import annotations

import argparse
import ast
import concurrent.futures
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SCOPES = {
    "examples": ["examples"],
    "stress": ["stress"],
    "valid": ["corpus/valid"],
    "all": ["examples", "stress", "corpus/valid"],
}
STANDALONE_TOML = """\
[project]
name = "astunit"
version = "0.1.0"
src = "src"
out = "build"

[python]
target = "3.13"

[emit]
class-default = "dataclass"
format = false

[strictness]
unintrospectable-dependency = "off"

[env]
required = []
"""
BUILD_ENV = {"TYC_NO_SYNC": "1", "TYC_NO_INTROSPECT": "1"}


def die(msg: str) -> "None":
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(2)


# ----------------------------------------------------------------- units --


def discover_units(corpus_root: Path, scope: str, pattern: str | None) -> list[str]:
    """Unit ids, relative to `corpus_root`: `dir/` for a project, a path for a file."""
    roots = [corpus_root / r for r in SCOPES[scope] if (corpus_root / r).is_dir()]
    if not roots:
        die(f"no corpus directories for scope '{scope}' under {corpus_root}")
    projects = sorted({p.parent for r in roots for p in r.rglob("typhon.toml")})
    units: list[str] = [p.relative_to(corpus_root).as_posix() + "/" for p in projects]
    for r in roots:
        for f in sorted(r.rglob("*.ty")):
            if not any(proj in f.parents for proj in projects):
                units.append(f.relative_to(corpus_root).as_posix())
    units = sorted(set(units))
    if pattern:
        rx = re.compile(pattern)
        units = [u for u in units if rx.search(u)]
    if not units:
        die("no units selected")
    return units


def slug(unit: str) -> str:
    return re.sub(r"[^A-Za-z0-9]+", "_", unit).strip("_")


# --------------------------------------------------------- normalisation --

_GENERATED = re.compile(r"(?<![A-Za-z0-9_])__typhon_[A-Za-z0-9_]*")


def canonicalise_generated_names(src: str) -> str:
    """Rename `__typhon_*` identifiers by first appearance (as ast_equiv.rs does)."""
    seen: dict[str, int] = {}

    def repl(m: re.Match[str]) -> str:
        name = m.group(0)
        if name not in seen:
            seen[name] = len(seen)
        return f"__typhon_c{seen[name]}__"

    return _GENERATED.sub(repl, src)


def _normalise_docstrings(tree: ast.AST) -> None:
    for node in ast.walk(tree):
        if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            body = node.body
            if (
                body
                and isinstance(body[0], ast.Expr)
                and isinstance(body[0].value, ast.Constant)
                and isinstance(body[0].value.value, str)
            ):
                text = body[0].value.value
                body[0].value.value = "\n".join(
                    line.strip() for line in text.splitlines() if line.strip()
                )


def ast_text(source: str, lenient_docstrings: bool) -> str:
    canonical = canonicalise_generated_names(source)
    tree = ast.parse(canonical)
    if lenient_docstrings:
        _normalise_docstrings(tree)
    try:
        return ast.dump(tree, indent=1)
    except RecursionError:
        # A few stress probes nest deeper than `ast.dump` can recurse even
        # with the raised limit below. Fall back to the canonicalised text
        # with comments and blank lines dropped: equal text still means an
        # equal program, and the marker keeps the two kinds apart.
        lines = [l.rstrip() for l in canonical.splitlines()]
        kept = [l for l in lines if l.strip() and not l.lstrip().startswith("#")]
        return "<ast too deep to dump; canonical source follows>\n" + "\n".join(kept)


# ------------------------------------------------------------------ dump --


def build_unit(tyc: Path, corpus_root: Path, unit: str, work: Path, timeout: int) -> tuple[str, str]:
    """Build one unit in `work`; return (status, detail)."""
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    if unit.endswith("/"):
        shutil.copytree(corpus_root / unit.rstrip("/"), work, dirs_exist_ok=True)
        for junk in ("build", ".venv"):
            shutil.rmtree(work / junk, ignore_errors=True)
    else:
        (work / "src").mkdir()
        shutil.copyfile(corpus_root / unit, work / "src" / "main.ty")
        (work / "typhon.toml").write_text(STANDALONE_TOML)
    env = {**os.environ, **BUILD_ENV}
    try:
        proc = subprocess.run(
            [str(tyc), "build", "--no-sync"],
            cwd=work,
            env=env,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return "timeout", f"tyc build exceeded {timeout}s"
    if proc.returncode != 0:
        return "nobuild", f"tyc build exit={proc.returncode}"
    return "built", ""


def dump_unit(args: tuple) -> dict:
    tyc, corpus_root, unit, scratch, timeout, lenient, blobs = args
    work = scratch / "w" / slug(unit)
    status, detail = build_unit(tyc, corpus_root, unit, work, timeout)
    files: dict[str, str] = {}
    if status == "built":
        out_dir = work / "build"
        for py in sorted(out_dir.rglob("*.py")):
            rel = py.relative_to(out_dir).as_posix()
            if rel.startswith(".venv/"):
                continue
            try:
                text = ast_text(py.read_text(encoding="utf-8"), lenient)
            except SyntaxError as e:
                text = f"<does not parse: {e}>"
            digest = hashlib.sha256(text.encode()).hexdigest()
            blob = blobs / digest[:2] / digest
            if not blob.exists():
                blob.parent.mkdir(parents=True, exist_ok=True)
                tmp = blob.with_suffix(f".{os.getpid()}.{slug(unit)}")
                tmp.write_text(text)
                os.replace(tmp, blob)
            files[rel] = digest
    shutil.rmtree(work, ignore_errors=True)
    return {"unit": unit, "status": status, "detail": detail, "files": files}


def dump(tyc: Path, out: Path, corpus_root: Path, units: list[str], jobs: int,
         timeout: int, lenient: bool, scratch: Path, label: str = "") -> None:
    if not os.access(tyc, os.X_OK):
        die(f"tyc binary not found or not executable: {tyc}")
    out.mkdir(parents=True, exist_ok=True)
    blobs = out / "blobs"
    blobs.mkdir(exist_ok=True)
    work_root = scratch / f"build-{slug(str(out))}"
    print(f"  {label or 'dump'}: building {len(units)} unit(s) with {tyc} …", flush=True)
    tasks = [(tyc, corpus_root, u, work_root, timeout, lenient, blobs) for u in units]
    results = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        for r in pool.map(dump_unit, tasks):
            results.append(r)
    meta = {"tyc": str(tyc), "corpus_root": str(corpus_root), "lenient_docstrings": lenient,
            "units": results}
    (out / "units.json").write_text(json.dumps(meta, indent=1))
    counts: dict[str, int] = {}
    for r in results:
        counts[r["status"]] = counts.get(r["status"], 0) + 1
    print("    " + ", ".join(f"{k}={v}" for k, v in sorted(counts.items())), flush=True)


# --------------------------------------------------------------- compare --


def _first_difference(a: str, b: str) -> str:
    al, bl = a.splitlines(), b.splitlines()
    for i, (x, y) in enumerate(zip(al, bl)):
        if x != y:
            return f"line {i + 1}: {x.strip()[:70]!r} vs {y.strip()[:70]!r}"
    return f"length {len(al)} vs {len(bl)} lines"


def compare(a: Path, b: Path, report: Path | None, label_a: str = "A", label_b: str = "B") -> int:
    ma = json.loads((a / "units.json").read_text())
    mb = json.loads((b / "units.json").read_text())
    if ma["lenient_docstrings"] != mb["lenient_docstrings"]:
        die("the two dumps were taken with different --lenient-docstrings settings")
    ua = {u["unit"]: u for u in ma["units"]}
    ub = {u["unit"]: u for u in mb["units"]}
    rows: list[tuple[str, str, str]] = []
    for unit in sorted(set(ua) | set(ub)):
        x, y = ua.get(unit), ub.get(unit)
        if x is None or y is None:
            rows.append(("missing", unit, f"only in {label_a if y is None else label_b}"))
            continue
        if x["status"] != y["status"]:
            rows.append(("status", unit, f"{label_a}={x['status']} {label_b}={y['status']}"))
            continue
        if x["status"] != "built":
            rows.append(("same-" + x["status"], unit, x["detail"]))
            continue
        fa, fb = x["files"], y["files"]
        diffs = []
        for rel in sorted(set(fa) | set(fb)):
            if rel not in fa or rel not in fb:
                diffs.append(f"{rel}: only in {label_a if rel in fa else label_b}")
            elif fa[rel] != fb[rel]:
                ta = (a / "blobs" / fa[rel][:2] / fa[rel]).read_text()
                tb = (b / "blobs" / fb[rel][:2] / fb[rel]).read_text()
                diffs.append(f"{rel}: {_first_difference(ta, tb)}")
        if diffs:
            rows.append(("changed", unit, "; ".join(diffs)))
        else:
            rows.append(("same", unit, f"{len(fa)} file(s)"))
    if report:
        report.write_text("".join(f"{v}\t{u}\t{d}\n" for v, u, d in rows))
    bad = [r for r in rows if r[0] in ("changed", "status", "missing")]
    counts: dict[str, int] = {}
    for v, _, _ in rows:
        counts[v] = counts.get(v, 0) + 1
    print("  verdicts: " + ", ".join(f"{k}={v}" for k, v in sorted(counts.items())))
    if bad:
        print(f"\n{len(bad)} unit(s) differ:")
        for v, u, d in bad:
            print(f"  [{v}] {u}\n      {d}")
        return 1
    print("  every unit emits the same Python AST on both sides")
    return 0


# ------------------------------------------------------------ resolving --


def resolve_compiler(spec: str, scratch: Path, name: str, target_dir: Path) -> Path:
    """`spec` is an executable tyc binary or a git revision to build."""
    p = Path(spec)
    if p.is_file() and os.access(p, os.X_OK):
        return p.resolve()
    rev = subprocess.run(["git", "rev-parse", "--verify", "--quiet", f"{spec}^{{commit}}"],
                         cwd=REPO_ROOT, capture_output=True, text=True)
    if rev.returncode != 0:
        die(f"'{spec}' is neither an executable tyc binary nor a git revision")
    sha = rev.stdout.strip()
    src = scratch / f"src-{name}"
    src.mkdir(parents=True)
    print(f"  {name}: exporting {spec} ({sha[:12]}) and building tyc …", flush=True)
    archive = subprocess.Popen(["git", "archive", sha], cwd=REPO_ROOT, stdout=subprocess.PIPE)
    untar = subprocess.run(["tar", "-x", "-C", str(src)], stdin=archive.stdout)
    archive.stdout.close()
    if archive.wait() != 0 or untar.returncode != 0:
        die(f"could not export {spec} with git archive")
    env = {**os.environ, "CARGO_TARGET_DIR": str(target_dir)}
    build = subprocess.run(["cargo", "build", "--release", "--bin", "tyc"], cwd=src / "tyc", env=env)
    if build.returncode != 0:
        die(f"cargo build failed for {spec}")
    exe = "tyc.exe" if os.name == "nt" else "tyc"
    out = scratch / f"tyc-{name}"
    shutil.copy2(target_dir / "release" / exe, out)
    return out


# --------------------------------------------------------- de-formatting --

_STRING_START = re.compile(r"""(?i)(?:rb|br|fr|rf|b|r|u|f)?('''|\"\"\"|'|")""")
# `=`-family operators: plain/augmented assignment, keyword `=`, walrus.
_ASSIGN_OP = re.compile(r"\s*(?<![=!<>])((?:\*\*|//|>>|<<|[-+*/%@&|^:])?=)(?!=)\s*")
_ARROW = re.compile(r"\s*->\s*")
_COMMA = re.compile(r",[ \t]+")
_COLON = re.compile(r":[ \t]+")


def _squeeze(code: str) -> str:
    code = _ASSIGN_OP.sub(lambda m: m.group(1), code)
    code = _ARROW.sub("->", code)
    code = _COMMA.sub(",", code)
    code = _COLON.sub(":", code)
    return code


def deformat(text: str) -> str:
    """Remove optional intra-line whitespace from code; never touch strings or comments.

    Indentation and line structure are kept, so the program means the same
    thing; only spacing a formatter would put back is taken away.
    """
    out_lines: list[str] = []
    quote: str | None = None  # open string delimiter carried across lines
    for line in text.split("\n"):
        # Indentation is significant; keep it unless the line continues a string.
        indent = "" if quote is not None else line[: len(line) - len(line.lstrip(" \t"))]
        i = len(indent)
        parts: list[tuple[bool, str]] = []  # (is_code, text)
        while i < len(line):
            if quote is not None:
                j, closed = i, False
                while j < len(line):
                    if line[j] == "\\":
                        j += 2
                        continue
                    if line.startswith(quote, j):
                        j += len(quote)
                        closed = True
                        break
                    j += 1
                parts.append((False, line[i:j]))
                i = j
                if closed:
                    quote = None
                    continue
                # A single-quoted string continues only past an escaped newline.
                if len(quote) == 1 and not line.endswith("\\"):
                    quote = None
                break
            m = _STRING_START.search(line, i)
            h = line.find("#", i)
            if h != -1 and (m is None or h < m.start()):
                parts.append((True, line[i:h]))
                parts.append((False, line[h:]))
                break
            if m is None:
                parts.append((True, line[i:]))
                break
            parts.append((True, line[i : m.start()]))
            parts.append((False, line[m.start() : m.end()]))
            quote = m.group(1)
            i = m.end()
        body = "".join(_squeeze(t) if is_code else t for is_code, t in parts)
        new = indent + body
        code_parts = parts
        # Trailing blanks on a plain code line (never after a line continuation,
        # never inside a string, never on a blank line).
        if quote is None and new.strip() and not new.endswith("\\") and code_parts and code_parts[-1][0]:
            new += "  "
        out_lines.append(new)
    return "\n".join(out_lines)


# -------------------------------------------------------------- commands --


def cmd_dump(ns: argparse.Namespace) -> int:
    corpus_root = Path(ns.corpus_root).resolve()
    units = discover_units(corpus_root, ns.scope, ns.filter)
    scratch = Path(tempfile.mkdtemp(prefix="emitted-ast."))
    try:
        dump(Path(ns.tyc).resolve(), Path(ns.out).resolve(), corpus_root, units, ns.jobs,
             ns.timeout, ns.lenient_docstrings, scratch)
    finally:
        if not ns.keep:
            shutil.rmtree(scratch, ignore_errors=True)
    return 0


def cmd_compare(ns: argparse.Namespace) -> int:
    return compare(Path(ns.a), Path(ns.b), Path(ns.report) if ns.report else None)


def cmd_equiv(ns: argparse.Namespace) -> int:
    corpus_root = Path(ns.corpus_root).resolve()
    units = discover_units(corpus_root, ns.scope, ns.filter)
    scratch = Path(tempfile.mkdtemp(prefix="emitted-ast."))
    target_dir = Path(ns.cargo_target_dir).resolve()
    print(f"emitted-AST equivalence: {ns.a}  vs  {ns.b}  ({len(units)} units, scope={ns.scope})")
    try:
        tyc_a = resolve_compiler(ns.a, scratch, "a", target_dir)
        tyc_b = resolve_compiler(ns.b, scratch, "b", target_dir)
        dump(tyc_a, scratch / "dump-a", corpus_root, units, ns.jobs, ns.timeout,
             ns.lenient_docstrings, scratch, "A")
        dump(tyc_b, scratch / "dump-b", corpus_root, units, ns.jobs, ns.timeout,
             ns.lenient_docstrings, scratch, "B")
        return compare(scratch / "dump-a", scratch / "dump-b",
                       Path(ns.report) if ns.report else None, ns.a, ns.b)
    finally:
        if ns.keep:
            print(f"  scratch kept at {scratch}")
        else:
            shutil.rmtree(scratch, ignore_errors=True)


def cmd_fmt_gate(ns: argparse.Namespace) -> int:
    tyc = Path(ns.tyc).resolve()
    if not os.access(tyc, os.X_OK):
        die(f"tyc binary not found or not executable: {tyc}")
    corpus_root = Path(ns.corpus_root).resolve()
    units = discover_units(corpus_root, ns.scope, ns.filter)
    scratch = Path(tempfile.mkdtemp(prefix="fmt-gate."))
    ruff = shutil.which("ruff")
    print(f"de-formatted corpus fmt gate ({len(units)} units, scope={ns.scope})")
    print(f"  tyc : {tyc}")
    print(f"  ruff: {ruff or 'not on PATH (tyc fmt runs its whitespace pass only)'}")
    try:
        # 1. The reference: what the corpus emits as written.
        dump(tyc, scratch / "before", corpus_root, units, ns.jobs, ns.timeout, True, scratch,
             "original")

        # 2. A copy of every unit's sources.
        tree = scratch / "tree"
        files: list[Path] = []
        for unit in units:
            src = corpus_root / unit.rstrip("/")
            dst = tree / unit.rstrip("/")
            if unit.endswith("/"):
                shutil.copytree(src, dst, ignore=shutil.ignore_patterns("build", ".venv"))
                files.extend(sorted(dst.rglob("*.ty")))
            else:
                dst.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(src, dst)
                files.append(dst)

        def fmt_one(job: tuple[Path, bool]) -> tuple[Path, bool, str]:
            """Run `tyc fmt` (or `--check`); return (path, refused, message)."""
            path, check = job
            cmd = [str(tyc), "fmt"] + (["--check"] if check else []) + [str(path)]
            try:
                p = subprocess.run(cmd, stdin=subprocess.DEVNULL, capture_output=True,
                                   text=True, timeout=ns.timeout)
            except subprocess.TimeoutExpired:
                return path, True, f"tyc fmt exceeded {ns.timeout}s"
            out = p.stderr + p.stdout
            # `--check` also exits non-zero for a file it would merely change;
            # only "could not be formatted" is a refusal.
            refused = p.returncode != 0 and (not check or "could not be formatted" in out)
            lines = [l.strip() for l in out.splitlines() if l.strip()]
            msg = next((l for l in lines if l.startswith(("×", "Error"))), lines[0] if lines else "")
            return path, refused, msg[:160]

        # Files `tyc fmt` cannot format even as written (parse-error probes)
        # are expected to be refused after de-formatting too.
        with concurrent.futures.ThreadPoolExecutor(max_workers=ns.jobs) as pool:
            already = {path for path, refused, _ in pool.map(fmt_one, [(f, True) for f in files])
                       if refused}

        # 3. De-format the copy, then `tyc fmt` it (one file per process, so
        #    every refusal is attributed).
        changed = 0
        for f in files:
            text = f.read_text(encoding="utf-8")
            new = deformat(text)
            if new != text:
                f.write_text(new, encoding="utf-8")
                changed += 1
        print(f"  de-formatted {changed} of {len(files)} .ty file(s)", flush=True)

        refused: list[tuple[str, str]] = []
        expected = 0
        with concurrent.futures.ThreadPoolExecutor(max_workers=ns.jobs) as pool:
            for path, was_refused, msg in pool.map(fmt_one, [(f, False) for f in files]):
                if not was_refused:
                    continue
                if path in already:
                    expected += 1
                else:
                    refused.append((path.relative_to(tree).as_posix(), msg))
        print(f"  tyc fmt: {len(files) - len(refused) - expected} formatted, {expected} refused "
              f"as written too (expected), {len(refused)} refused only after de-formatting",
              flush=True)

        # 4. What the formatted copy emits.
        dump(tyc, scratch / "after", tree, units, ns.jobs, ns.timeout, True, scratch,
             "de-formatted + tyc fmt")
        rc = compare(scratch / "before", scratch / "after",
                     Path(ns.report) if ns.report else None, "original", "formatted")
        if refused:
            print(f"\n{len(refused)} file(s) `tyc fmt` formats as written but refused after "
                  "de-formatting:")
            for rel, msg in refused:
                print(f"  {rel}\n      {msg}")
            rc = 1
        return rc
    finally:
        if ns.keep:
            print(f"  scratch kept at {scratch}")
        else:
            shutil.rmtree(scratch, ignore_errors=True)


def main() -> int:
    if sys.version_info < (3, 13):
        die("run this with CPython 3.13+ (the emitted code uses 3.13 syntax)")
    # Stress probes nest expressions a few hundred levels deep; `ast.dump`
    # recurses once per level, on worker threads whose default stack is small.
    sys.setrecursionlimit(50_000)
    threading.stack_size(256 * 1024 * 1024)
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    def common(sp: argparse.ArgumentParser) -> None:
        sp.add_argument("--scope", choices=sorted(SCOPES), default="all")
        sp.add_argument("--filter")
        sp.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
        sp.add_argument("--timeout", type=int, default=60)
        sp.add_argument("--corpus-root", default=str(REPO_ROOT))
        sp.add_argument("--keep", action="store_true")
        sp.add_argument("--report")

    sp = sub.add_parser("equiv", help="compare two compilers (binaries or git revisions)")
    sp.add_argument("a")
    sp.add_argument("b")
    sp.add_argument("--cargo-target-dir", default=str(REPO_ROOT / "tyc" / "target" / "emitted-ast"))
    sp.add_argument("--lenient-docstrings", action="store_true")
    common(sp)
    sp.set_defaults(func=cmd_equiv)

    sp = sub.add_parser("fmt-gate", help="the de-formatted-corpus tyc fmt gate")
    sp.add_argument("--tyc", default=str(REPO_ROOT / "tyc" / "target" / "release" / "tyc"))
    common(sp)
    sp.set_defaults(func=cmd_fmt_gate)

    sp = sub.add_parser("dump", help="record the emitted AST of every unit")
    sp.add_argument("--tyc", required=True)
    sp.add_argument("--out", required=True)
    sp.add_argument("--lenient-docstrings", action="store_true")
    common(sp)
    sp.set_defaults(func=cmd_dump)

    sp = sub.add_parser("compare", help="compare two dumps")
    sp.add_argument("a")
    sp.add_argument("b")
    sp.add_argument("--report")
    sp.set_defaults(func=cmd_compare)

    ns = p.parse_args()
    return ns.func(ns)


if __name__ == "__main__":
    sys.exit(main())
