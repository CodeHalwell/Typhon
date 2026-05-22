#!/bin/bash
# Run every .ty in every numbered subdir. Summarises pass/fail.
set -u
ROOT="$(cd "$(dirname "$0")" && pwd)"
mut_pass=0
mut_fail=0
FAILED=()
mut_skipped=0

for ty in $(find "$ROOT" -name "*.ty" -path "*/0*-*/*" -not -path "*/build/*" | sort); do
    out="$(RUN=${RUN:-1} PYTHON=${PYTHON:-/tmp/typhonvenv/bin/python} "$ROOT/run_one.sh" "$ty" 2>&1)"
    last="$(echo "$out" | tail -1)"
    rel="${ty#$ROOT/}"
    if [ "$last" = "[OK]" ]; then
        mut_pass=$((mut_pass + 1))
        echo "PASS: $rel"
    else
        mut_fail=$((mut_fail + 1))
        FAILED+=("$rel: $last")
        echo "FAIL: $rel — $last"
    fi
done

echo "---"
echo "PASS: $mut_pass"
echo "FAIL: $mut_fail"
echo "---"
if [ ${#FAILED[@]} -gt 0 ]; then
    echo "Failures:"
    printf '  %s\n' "${FAILED[@]}"
fi
