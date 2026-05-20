#!/usr/bin/env bash
# Build and run a single .ty file. Outputs:
#   build status (BUILD_OK / BUILD_FAIL)
#   if BUILD_OK: run status (RUN_OK / RUN_FAIL exit=N)
#   if requested: emitted python on stdout
set -u
TYC="${TYC:-/home/user/Typhon/tyc/target/release/tyc}"
PY="${PYTHON:-/usr/bin/python3.13}"
SHOW_EMIT="${SHOW_EMIT:-0}"
file="${1:?Usage: $0 <file.ty>}"
name=$(basename "$file" .ty)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/src"
cp "$file" "$work/src/main.ty"
cat > "$work/typhon.toml" <<EOF
[project]
name = "case"
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
methods-in-class-body = "warn"
EOF
echo "=== $name ==="
build_out=$(cd "$work" && "$TYC" build 2>&1)
build_exit=$?
if [ $build_exit -ne 0 ]; then
  echo "BUILD_FAIL"
  echo "$build_out" | head -20
  exit 1
fi
echo "BUILD_OK"
if [ "$SHOW_EMIT" = "1" ]; then
  echo "--- emitted ---"
  cat "$work/build/main.py"
  echo "--- end emit ---"
fi
run_out=$(cd "$work" && "$PY" build/main.py 2>&1)
run_exit=$?
if [ $run_exit -ne 0 ]; then
  echo "RUN_FAIL exit=$run_exit"
  echo "$run_out" | head -25
  exit 2
fi
echo "RUN_OK"
echo "$run_out" | head -25
exit 0
