#!/usr/bin/env bash
# Verify third-party (numpy / pandas / scikit-learn) argument checking.
#
# REQUIRES a project `.venv` with the libraries installed (the dist→import
# resolution and `inspect.signature` introspection both read the venv):
#
#   python3.13 -m venv .venv && .venv/bin/pip install numpy pandas scikit-learn
#
# Each `must_fail/*.ty` must fail `tyc check` with tyc::missing_argument
# (naming the missing parameter); each `must_pass/*.ty` must check clean.
#
# Overridable: TYC=/path/to/tyc
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
TYC=${TYC:-"$REPO_ROOT/tyc/target/release/tyc"}
PROJ="$SCRIPT_DIR/proj"

mkdir -p "$PROJ/src"
cat > "$PROJ/typhon.toml" <<'TOML'
[project]
name = "tpcheck"
version = "0.1.0"
src = "src"
out = "build"
[python]
target = "3.13"
[dependencies]
numpy = "*"
pandas = "*"
scikit-learn = "*"
TOML
if [ ! -x "$PROJ/.venv/bin/python" ]; then
  echo "NOTE: creating $PROJ/.venv (installing numpy/pandas/scikit-learn)…"
  python3.13 -m venv "$PROJ/.venv" >/dev/null 2>&1
  "$PROJ/.venv/bin/pip" install --quiet numpy pandas scikit-learn || {
    echo "FATAL: could not install libs into $PROJ/.venv"; exit 2; }
fi

fail_ok=0; fail_bad=0; pass_ok=0; pass_bad=0
for f in "$SCRIPT_DIR"/must_fail/*.ty; do
  cp "$f" "$PROJ/src/main.ty"
  out=$(cd "$PROJ" && "$TYC" check src/main.ty 2>&1)
  if echo "$out" | grep -q "tyc::missing_argument"; then
    msg=$(echo "$out" | grep -i "supply" | head -1 | sed 's/^ *//')
    echo "[FAIL-OK] $(basename "$f") — $msg"; fail_ok=$((fail_ok+1))
  else
    echo "[FAIL-MISS] $(basename "$f") — expected missing_argument, got none"; fail_bad=$((fail_bad+1))
  fi
done
for f in "$SCRIPT_DIR"/must_pass/*.ty; do
  cp "$f" "$PROJ/src/main.ty"
  out=$(cd "$PROJ" && "$TYC" check src/main.ty 2>&1)
  if echo "$out" | grep -qE "[1-9][0-9]* error\(s\)"; then
    echo "[PASS-FALSEPOS] $(basename "$f")"; echo "$out" | grep -iE "supply|type_mismatch|missing" | head -2
    pass_bad=$((pass_bad+1))
  else
    echo "[PASS-OK] $(basename "$f")"; pass_ok=$((pass_ok+1))
  fi
done
echo
echo "must_fail: caught=$fail_ok missed=$fail_bad | must_pass: clean=$pass_ok false_positive=$pass_bad"
