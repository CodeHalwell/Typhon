#!/usr/bin/env bash
# Build and run a single .ty file under a transient project.
set -u
TYC="${TYC:-/home/user/Typhon/tyc/target/release/tyc}"
PY="${PYTHON:-python3.13}"
file="$1"
work=$(mktemp -d)
trap "rm -rf $work" EXIT
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
format = true

[strictness]
no-implicit-any = true
unused-import = "error"
exhaustive-match = "error"
EOF
echo "=== build: $file ==="
(cd "$work" && "$TYC" build 2>&1)
build_exit=$?
echo "(build exit: $build_exit)"
if [ $build_exit -eq 0 ]; then
  echo "=== emitted: $work/build/main.py ==="
  cat "$work/build/main.py"
  echo "=== run ==="
  (cd "$work" && "$PY" build/main.py 2>&1)
  echo "(run exit: $?)"
fi
