#!/bin/bash
# Build each .ty argument as a tyc project and run the emitted Python.
# Overridable:
#   TYC=/path/to/tyc       (default: <repo>/tyc/target/release/tyc)
#   PYTHON=python3.13      (default: python3.13 — Typhon targets 3.13+)
TYC=${TYC:-"$(git rev-parse --show-toplevel 2>/dev/null)/tyc/target/release/tyc"}
PYTHON=${PYTHON:-python3.13}
if [ ! -x "$TYC" ]; then
    echo "tyc binary not found at $TYC — set TYC=... or build with 'cargo build --release' in tyc/" >&2
    exit 1
fi
mkdir -p builds
for tc in "$@"; do
    name=$(basename "$tc" .ty)
    if [ -z "$name" ]; then
        echo "skipping '$tc' — empty basename would resolve workdir to 'builds/'" >&2
        continue
    fi
    workdir="builds/$name"
    rm -rf "$workdir"
    mkdir -p "$workdir/src"
    cp "$tc" "$workdir/src/main.ty"
    cat > "$workdir/typhon.toml" <<TOML
[project]
name = "$name"
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
[env]
required = []
TOML
    pushd "$workdir" > /dev/null
    "$TYC" build > build.out 2>&1
    bcode=$?
    if [ $bcode -eq 0 ]; then
        "$PYTHON" build/main.py > run.out 2>&1
        rcode=$?
        echo "$name: build=$bcode run=$rcode"
    else
        echo "$name: build=$bcode FAILED"
    fi
    popd > /dev/null
done
