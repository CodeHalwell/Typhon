#!/bin/bash
# Run tyc check/build on every .ty file in tests/ and capture results.
# Override TYC to point at a different binary; default is the repo's release build.
TYC=${TYC:-"$(git rev-parse --show-toplevel 2>/dev/null)/tyc/target/release/tyc"}
if [ ! -x "$TYC" ]; then
    echo "tyc binary not found at $TYC — set TYC=... or build with 'cargo build --release' in tyc/" >&2
    exit 1
fi
mkdir -p out
for f in tests/*.ty; do
    name=$(basename "$f" .ty)
    echo "=== $name ===" >&2
    "$TYC" check "$f" > "out/$name.check.out" 2>&1
    code=$?
    echo "[check exit=$code]" > "out/$name.summary"
done
