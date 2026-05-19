#!/usr/bin/env bash
# Run tyc check on a single .ty file; show full output (no truncation).
set -u
TYC="${TYC:-/home/user/Typhon/tyc/target/release/tyc}"
file="$1"
echo "=== $file ==="
"$TYC" check "$file" 2>&1
echo "(exit: $?)"
