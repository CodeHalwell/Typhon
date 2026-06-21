#!/usr/bin/env bash
# Wide third-party introspection harness (2026-06-21 round).
#
# Classifies every repro into one of four buckets:
#   must_pass/*.ty  -> PASS-OK (clean) | PASS-FALSEPOS (any error = critical bug)
#   must_fail/*.ty  -> FAIL-CAUGHT (fires the expected tyc:: code) | FAIL-MISSED
#
# REQUIRES a project `.venv` with the libraries installed — the dist->import
# resolution and `inspect.signature` introspection both read it:
#
#   python3.13 -m venv proj/.venv
#   proj/.venv/bin/pip install -r requirements.txt
#
# `proj/` (and its `.venv`/`build/`) is generated and MUST NOT be committed.
#
# Overridable: TYC=/path/to/tyc
set -u
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
TYC=${TYC:-"$REPO_ROOT/tyc/target/release/tyc"}
PROJ="$SCRIPT_DIR/proj"

if [ ! -x "$PROJ/.venv/bin/python" ]; then
  echo "FATAL: $PROJ/.venv not found. Create it and install requirements.txt first"
  echo "  python3.13 -m venv $PROJ/.venv && $PROJ/.venv/bin/pip install -r $SCRIPT_DIR/requirements.txt"
  exit 2
fi
mkdir -p "$PROJ/src"

# Generate proj/typhon.toml from requirements.txt so the corpus is
# self-contained (proj/ itself is gitignored). Each requirement's PyPI name
# becomes a `[dependencies]` key — that drives the introspection allow-list
# and the dist->import name resolution.
{
  printf '[project]\nname = "tpwide"\nversion = "0.1.0"\nsrc = "src"\nout = "build"\n'
  printf '[python]\ntarget = "3.13"\n[dependencies]\n'
  sed 's/[=<>!~].*//' "$SCRIPT_DIR/requirements.txt" | sed '/^[[:space:]]*$/d' \
    | while read -r dep; do printf '%s = "*"\n' "$dep"; done
} > "$PROJ/typhon.toml"

run_check() { cp "$1" "$PROJ/src/main.ty"; (cd "$PROJ" && "$TYC" check src/main.ty 2>&1); }
has_error() { echo "$1" | grep -qE "[1-9][0-9]* error\(s\)"; }

pass_ok=0; pass_fp=0; fail_caught=0; fail_missed=0
echo "===== must_pass (idiomatic; any error is a FALSE POSITIVE) ====="
for f in "$SCRIPT_DIR"/must_pass/*.ty; do
  out=$(run_check "$f")
  if has_error "$out"; then
    echo "[PASS-FALSEPOS] $(basename "$f")"
    echo "$out" | grep -iE "tyc::" | grep -v main_not_called | head -2
    pass_fp=$((pass_fp+1))
  else
    echo "[PASS-OK] $(basename "$f")"; pass_ok=$((pass_ok+1))
  fi
done
echo
echo "===== must_fail (must fire missing_argument / type_mismatch / unknown_kwarg) ====="
for f in "$SCRIPT_DIR"/must_fail/*.ty; do
  out=$(run_check "$f")
  code=$(echo "$out" | grep -oE "tyc::(missing_argument|type_mismatch|unknown_kwarg)" | head -1)
  if [ -n "$code" ]; then
    echo "[FAIL-CAUGHT] $(basename "$f") -> $code"; fail_caught=$((fail_caught+1))
  else
    echo "[FAIL-MISSED] $(basename "$f")"; fail_missed=$((fail_missed+1))
  fi
done
echo
echo "===== SUMMARY ====="
echo "must_pass: ok=$pass_ok false_positive=$pass_fp"
echo "must_fail: caught=$fail_caught missed=$fail_missed"
