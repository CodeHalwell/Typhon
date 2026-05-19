#!/bin/bash
# Run tyc check/build on every .ty file in tests/ and capture results.
# Override TYC to point at a different binary; default is the repo's release build.
TYC=${TYC:-"$(git rev-parse --show-toplevel 2>/dev/null)/tyc/target/release/tyc"}
if [ ! -x "$TYC" ]; then
    echo "tyc binary not found at $TYC — set TYC=... or build with 'cargo build --release' in tyc/" >&2
    exit 1
fi
mkdir -p out
shopt -s nullglob
files=(tests/*.ty)
if [ "${#files[@]}" -eq 0 ]; then
    echo "no .ty files found in tests/" >&2
    exit 1
fi
for f in "${files[@]}"; do
    name=$(basename "$f" .ty)
    if [ -z "$name" ]; then
        echo "skipping '$f' — empty basename" >&2
        continue
    fi
    echo "=== $name ===" >&2
    "$TYC" check "$f" > "out/$name.check.out" 2>&1
    code=$?
    echo "[check exit=$code]" > "out/$name.summary"
done
