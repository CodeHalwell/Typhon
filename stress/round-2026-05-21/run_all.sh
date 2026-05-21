#!/bin/bash
# Run every .ty file in the round, summarising pass/fail.
set -u
ROOT="$(cd "$(dirname "$0")" && pwd)"
PASS=0
FAIL=0
SKIP=0
FAILED_NAMES=()
LOG="$ROOT/run_all.log"
: > "$LOG"

shopt -s nullglob
for dir in "$ROOT"/[0-9][0-9]-*/; do
    for ty in "$dir"*.ty; do
        rel="${ty#$ROOT/}"
        echo "---- $rel ----" >> "$LOG"
        if out="$("$ROOT/run_one.sh" "$ty" 2>&1)"; then
            echo "PASS  $rel"
            echo "$out" >> "$LOG"
            PASS=$((PASS+1))
        else
            echo "FAIL  $rel"
            echo "$out" >> "$LOG"
            FAIL=$((FAIL+1))
            FAILED_NAMES+=("$rel")
        fi
    done
done
echo ""
echo "Pass: $PASS  Fail: $FAIL"
if [ $FAIL -gt 0 ]; then
    echo "Failed:"
    for n in "${FAILED_NAMES[@]}"; do echo "  $n"; done
fi
