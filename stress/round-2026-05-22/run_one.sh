#!/bin/bash
# Build a single .ty file (no run by default). Writes to build/<basename>/.
# Set RUN=1 to also exec the emitted Python.
# Set SHOW_EMIT=1 to print emitted .py on success.
set -u
TY="$1"
NAME="$(basename "${TY%.ty}")"
ROOT="$(cd "$(dirname "$0")" && pwd)"
OUT="$ROOT/build/$NAME"
TYC="${TYC:-$ROOT/../../tyc/target/release/tyc}"

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
export TYC_NO_SYNC=1
BUILD_OUT="$("$TYC" build 2>&1)"
BUILD_EXIT=$?
echo "$BUILD_OUT" | head -60
if [ $BUILD_EXIT -ne 0 ]; then
    echo "[BUILD FAIL exit=$BUILD_EXIT]"
    exit 1
fi
if [ "${SHOW_EMIT:-0}" = "1" ]; then
    echo "---- emitted ----"
    cat out/main.py
    echo "---- end emitted ----"
fi
if [ "${RUN:-0}" = "1" ]; then
    PYTHON="${PYTHON:-python3.13}"
    RUN_OUT="$("$PYTHON" out/main.py 2>&1)"
    RUN_EXIT=$?
    echo "$RUN_OUT" | head -40
    if [ $RUN_EXIT -ne 0 ]; then
        echo "[RUN FAIL exit=$RUN_EXIT]"
        exit 2
    fi
fi
echo "[OK]"
