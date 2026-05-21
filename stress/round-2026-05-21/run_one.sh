#!/bin/bash
# Build + run a single .ty file. Writes to build/<basename>/.
set -u
TY="$1"
NAME="$(basename "${TY%.ty}")"
ROOT="$(cd "$(dirname "$0")" && pwd)"
OUT="$ROOT/build/$NAME"
TYC="$ROOT/../../tyc/target/release/tyc"

rm -rf "$OUT"
mkdir -p "$OUT/src"
cp "$TY" "$OUT/src/main.ty"

cat > "$OUT/typhon.toml" <<EOF
[project]
name = "$NAME"
version = "0.1.0"
src = "src"
out = "out"

[python]
target = "3.13"

[emit]
class-default = "dataclass"
format = false

[strictness]
no-implicit-any = true
unused-import = "warn"
EOF

# Allow the test to add deps via a sidecar `.deps` file (one package per line).
DEPS_FILE="${TY%.ty}.deps"
if [ -f "$DEPS_FILE" ]; then
    {
        echo ""
        echo "[dependencies]"
        while read -r dep; do
            [ -z "$dep" ] && continue
            echo "$dep = \"*\""
        done < "$DEPS_FILE"
    } >> "$OUT/typhon.toml"
fi

echo "==== $TY ===="
cd "$OUT" || exit 2
BUILD_OUT="$("$TYC" build 2>&1)"
BUILD_EXIT=$?
echo "$BUILD_OUT" | head -40
if [ $BUILD_EXIT -ne 0 ]; then
    echo "[BUILD FAIL exit=$BUILD_EXIT]"
    exit 1
fi
if [ "${SHOW_EMIT:-0}" = "1" ]; then
    echo "---- emitted ----"
    cat out/main.py
    echo "---- end emitted ----"
fi
PYTHON="$OUT/.venv/bin/python"
if [ ! -x "$PYTHON" ]; then PYTHON="python3.13"; fi
RUN_OUT="$("$PYTHON" out/main.py 2>&1)"
RUN_EXIT=$?
echo "$RUN_OUT" | head -40
if [ $RUN_EXIT -ne 0 ]; then
    echo "[RUN FAIL exit=$RUN_EXIT]"
    exit 2
fi
echo "[OK]"
