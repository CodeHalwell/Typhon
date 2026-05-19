#!/bin/bash
# Run tyc check/build on every .ty file in tests/ and capture results
TYC=/home/user/Typhon/tyc/target/release/tyc
mkdir -p out
for f in tests/*.ty; do
    name=$(basename "$f" .ty)
    echo "=== $name ===" >&2
    "$TYC" check "$f" > "out/$name.check.out" 2>&1
    code=$?
    echo "[check exit=$code]" > "out/$name.summary"
done
