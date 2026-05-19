#!/usr/bin/env bash
# Run tyc check on a single .ty file; show full output (no truncation).
# Override TYC to point at a different binary; default is the repo's release build.
set -u
TYC="${TYC:-$(git rev-parse --show-toplevel 2>/dev/null)/tyc/target/release/tyc}"
if [ ! -x "$TYC" ]; then
    echo "tyc binary not found at $TYC — set TYC=... or build with 'cargo build --release' in tyc/" >&2
    exit 1
fi
file="$1"
echo "=== $file ==="
"$TYC" check "$file" 2>&1
echo "(exit: $?)"
