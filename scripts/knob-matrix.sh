#!/usr/bin/env bash
# knob-matrix.sh — coverage for Typhon's opt-in codegen paths (review item T0.4).
#
# Every knob under `[strictness]`, `[optimise]`, and the codegen-affecting
# `[emit]` / `[python]` keys changes the Python `tyc build` writes. Before this
# harness, none of those rewrites were exercised end-to-end anywhere: the corpus
# in `examples/` and `stress/` runs entirely on default configuration, so the
# auto-parallel rewrite, the parallel backends, PGO memoisation, the PEP 810
# lazy-import lowering and traceback remapping shipped with zero executed tests.
#
# Each fixture in tests/knobs/<name>/ is a complete miniature project. For each
# one the harness asserts, in order:
#
#   1. the knob-ON build succeeds;
#   2. every marker in emit-contains.txt appears in the emitted Python, and
#      every marker in emit-absent.txt does not          → the knob fired;
#   3. with control.toml (the same project, knob OFF) every emit-contains.txt
#      marker is GONE, and every control-contains.txt marker is back
#                                                        → the assertion in (2)
#      is actually knob-sensitive and not vacuously true;
#   4. the knob-ON build runs under python3.13 with exactly the expected stdout
#      and exit code (plus any stderr substrings)        → the rewrite is correct;
#   5. the knob-OFF build produces byte-identical stdout → the rewrite is
#      semantics-preserving, which is the whole promise of an "opt-in
#      optimisation";
#   6. `tyc run` (the VM) agrees with CPython — or diverges, when the fixture
#      declares `vm-diverges=yes`, in which case *agreement* is the failure
#      (so that allowance cannot rot either).
#
# A fixture may declare `requires-module=NAME` in meta.conf when its *runtime*
# half needs a third-party package (its codegen half never does). If that module
# is missing the fixture drops to build-only and says so on its own result line
# AND in the summary — never silently, because a skip nothing observes is
# indistinguishable from a pass. Nothing here reaches the network.
#
# Needs bash, coreutils, a release `tyc`, and python3.13.
#
# Fixture files (all optional except typhon.toml + src/):
#   typhon.toml           knob ON
#   control.toml          same project, knob OFF (enables checks 3 and 5)
#   emit-contains.txt     substrings required in the ON build's emitted Python
#   emit-absent.txt       substrings forbidden in the ON build
#   control-contains.txt  substrings required in the control build
#   expect.txt            exact expected CPython stdout
#   stderr-contains.txt   substrings required in CPython stderr
#   typhon-profile.json   committed profile data (pgo-memoise)
#   meta.conf             run=both|none, expect-exit=N, vm-diverges=yes,
#                         requires-module=NAME
# In a marker file, a literal `\n` is expanded to a real newline so one marker
# can span source lines.
#
# Usage:
#   scripts/knob-matrix.sh [--filter REGEX] [--keep]
#
# Environment: TYC, PYTHON313, TYC_REQUIRE_PYTHON (see vm-differential.sh).

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

TYC="${TYC:-$REPO_ROOT/tyc/target/release/tyc}"
PYTHON313="${PYTHON313:-python3.13}"
FIXTURES="$REPO_ROOT/tests/knobs"
FILTER=""
KEEP=0

while [ $# -gt 0 ]; do
    case "$1" in
        --filter) FILTER="$2"; shift 2 ;;
        --keep)   KEEP=1; shift ;;
        -h|--help) sed -n '2,58p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

if [ ! -x "$TYC" ]; then
    echo "error: tyc binary not found at '$TYC'." >&2
    echo "       build it with: (cd tyc && cargo build --release)" >&2
    exit 2
fi
if ! command -v "$PYTHON313" >/dev/null 2>&1; then
    if [ "${TYC_REQUIRE_PYTHON:-0}" = "1" ]; then
        echo "error: '$PYTHON313' not found and TYC_REQUIRE_PYTHON=1." >&2
        exit 2
    fi
    echo "error: '$PYTHON313' not found. Typhon emits CPython 3.13+; python3" >&2
    echo "       ($(python3 --version 2>&1)) is not a substitute. Refusing to run" >&2
    echo "       a harness that would pass vacuously." >&2
    exit 2
fi

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/tyc-knob.XXXXXX")"
cleanup() { [ "$KEEP" = "1" ] || rm -rf "$SCRATCH"; }
trap cleanup EXIT

mapfile -t NAMES < <(find "$FIXTURES" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort)
if [ -n "$FILTER" ]; then
    mapfile -t NAMES < <(printf '%s\n' "${NAMES[@]}" | grep -E "$FILTER")
fi
if [ "${#NAMES[@]}" -eq 0 ]; then
    echo "error: no knob fixtures selected under tests/knobs (filter='$FILTER')." >&2
    exit 2
fi

echo "opt-in knob codegen matrix"
echo "  tyc      : $TYC"
echo "  python   : $PYTHON313 ($("$PYTHON313" --version 2>&1))"
echo "  fixtures : ${#NAMES[@]}"
echo

meta() { # meta <fixture-dir> <key> <default>
    local f="$1/meta.conf"
    [ -f "$f" ] || { printf '%s' "$3"; return; }
    local v
    v="$(grep -E "^$2=" "$f" | tail -1 | cut -d= -f2-)"
    printf '%s' "${v:-$3}"
}

# assert_markers <label> <file-of-substrings> <haystack-file> <present|absent>
# One marker per line. A literal `\n` inside a marker becomes a real newline, so
# a marker can span lines (e.g. a decorator plus the def it sits on).
assert_markers() {
    local label="$1" list="$2" hay="$3" mode="$4" bad=0
    [ -f "$list" ] || return 0
    # Flatten newlines to \x01 on both sides so a multi-line marker is a plain
    # single-line substring search. (`grep -z` cannot be used: it still splits
    # the *pattern* on newlines, turning one marker into several alternatives.)
    local flat="$SCRATCH/.hay.flat"
    tr '\n' '\001' < "$hay" > "$flat"
    while IFS= read -r raw; do
        [ -z "$raw" ] && continue
        case "$raw" in \#*) continue ;; esac
        local m; m="$(printf '%b' "$raw" | tr '\n' '\001')"
        if grep -qF -- "$m" "$flat"; then
            [ "$mode" = "absent" ] && { echo "      $label: marker unexpectedly PRESENT: $raw"; bad=1; }
        else
            [ "$mode" = "present" ] && { echo "      $label: marker MISSING: $raw"; bad=1; }
        fi
    done < "$list"
    return $bad
}

PASS=0; FAIL=0; FAILED_NAMES=(); REDUCED_NAMES=()

for name in "${NAMES[@]}"; do
    fx="$FIXTURES/$name"
    printf '  %-32s ' "$name"
    problems=""

    run_mode="$(meta "$fx" run both)"
    want_exit="$(meta "$fx" expect-exit 0)"
    vm_diverges="$(meta "$fx" vm-diverges no)"
    needs_mod="$(meta "$fx" requires-module "")"
    reduced=""

    # A fixture may need a third-party module to *execute* (its codegen half
    # never does). Rather than skip silently — a skip nothing observes reads as
    # a pass — drop to build-only and say so on the fixture's own result line,
    # and again in the summary.
    if [ -n "$needs_mod" ] && [ "$run_mode" != "none" ]; then
        if ! "$PYTHON313" -c "import $needs_mod" >/dev/null 2>&1; then
            run_mode="none"
            reduced=" (build-only: $needs_mod not installed for $PYTHON313)"
        fi
    fi

    # ---- knob-ON build -----------------------------------------------------
    on="$SCRATCH/$name/on"
    mkdir -p "$on"; cp -r "$fx/." "$on/"
    rm -rf "$on/build" "$on/control.toml" "$on/meta.conf" \
           "$on"/*.txt 2>/dev/null
    ( cd "$on" && TYC_NO_SYNC=1 TYC_NO_INTROSPECT=1 \
        "$TYC" build --no-sync >build.log 2>&1 )
    if [ $? -ne 0 ]; then
        echo "FAIL"; echo "      knob-ON build failed:"; sed 's/^/        /' "$on/build.log" | head -20
        FAIL=$((FAIL+1)); FAILED_NAMES+=("$name"); continue
    fi
    find "$on/build" -name '*.py' -print0 | sort -z | xargs -0 cat > "$SCRATCH/$name/on.emit"

    assert_markers "ON" "$fx/emit-contains.txt" "$SCRATCH/$name/on.emit" present || problems="y"
    assert_markers "ON" "$fx/emit-absent.txt"   "$SCRATCH/$name/on.emit" absent  || problems="y"

    # ---- control (knob-OFF) build -----------------------------------------
    have_control=0
    if [ -f "$fx/control.toml" ]; then
        have_control=1
        off="$SCRATCH/$name/off"
        mkdir -p "$off"; cp -r "$fx/." "$off/"
        rm -rf "$off/build" "$off/meta.conf" "$off"/*.txt 2>/dev/null
        mv "$off/control.toml" "$off/typhon.toml"
        ( cd "$off" && TYC_NO_SYNC=1 TYC_NO_INTROSPECT=1 \
            "$TYC" build --no-sync >build.log 2>&1 )
        if [ $? -ne 0 ]; then
            echo "      control build failed:"; sed 's/^/        /' "$off/build.log" | head -10
            problems="y"; have_control=0
        else
            find "$off/build" -name '*.py' -print0 | sort -z | xargs -0 cat > "$SCRATCH/$name/off.emit"
            # Everything the knob ADDS must be gone with the knob off — that is
            # what makes the ON assertions provably knob-sensitive rather than
            # trivially true. `control-contains.txt` states the reverse for
            # knobs that REMOVE something (e.g. skip-decoration-bases).
            assert_markers "CONTROL" "$fx/emit-contains.txt"   "$SCRATCH/$name/off.emit" absent  || problems="y"
            assert_markers "CONTROL" "$fx/control-contains.txt" "$SCRATCH/$name/off.emit" present || problems="y"
        fi
    fi

    if [ "$run_mode" = "none" ]; then
        if [ -z "$problems" ]; then
            echo "ok (build-only)${reduced}"; PASS=$((PASS+1))
            [ -n "$reduced" ] && REDUCED_NAMES+=("$name")
        else echo "FAIL"; FAIL=$((FAIL+1)); FAILED_NAMES+=("$name"); fi
        continue
    fi

    # ---- execute the knob-ON build under CPython ---------------------------
    env_pre=(env PYTHONHASHSEED=0 PYTHONDONTWRITEBYTECODE=1 TZ=UTC LC_ALL=C.UTF-8)
    ( cd "$on" && "${env_pre[@]}" timeout 60 "$PYTHON313" build/main.py \
        </dev/null >run.out 2>run.err )
    got_exit=$?
    if [ "$got_exit" != "$want_exit" ]; then
        echo "      CPython exit=$got_exit, expected $want_exit"
        sed 's/^/        /' "$on/run.err" | head -8
        problems="y"
    fi
    if [ -f "$fx/expect.txt" ] && ! diff -q "$fx/expect.txt" "$on/run.out" >/dev/null; then
        echo "      CPython stdout differs from expect.txt:"
        diff "$fx/expect.txt" "$on/run.out" | sed 's/^/        /' | head -12
        problems="y"
    fi
    assert_markers "STDERR" "$fx/stderr-contains.txt" "$on/run.err" present || problems="y"

    # ---- the rewrite must not change observable behaviour ------------------
    if [ "$have_control" = "1" ]; then
        ( cd "$off" && "${env_pre[@]}" timeout 60 "$PYTHON313" build/main.py \
            </dev/null >run.out 2>run.err )
        off_exit=$?
        if ! diff -q "$on/run.out" "$off/run.out" >/dev/null || [ "$off_exit" != "$got_exit" ]; then
            echo "      knob changed observable behaviour (knob-OFF exit=$off_exit vs ON exit=$got_exit):"
            diff "$on/run.out" "$off/run.out" | sed 's/^/        /' | head -12
            problems="y"
        fi
    fi

    # ---- VM parity ---------------------------------------------------------
    if [ "$run_mode" = "both" ]; then
        ( cd "$on" && "${env_pre[@]}" TYC_NO_INTROSPECT=1 timeout 60 \
            "$TYC" run . </dev/null >vm.out 2>vm.err )
        vm_exit=$?
        if diff -q "$on/run.out" "$on/vm.out" >/dev/null && [ "$vm_exit" = "$got_exit" ]; then
            if [ "$vm_diverges" = "yes" ]; then
                echo "      fixture declares vm-diverges=yes but the VM now AGREES —"
                echo "      remove 'vm-diverges=yes' from meta.conf (the allowance must not rot)."
                problems="y"
            fi
        else
            if [ "$vm_diverges" != "yes" ]; then
                echo "      VM disagrees with CPython (vm exit=$vm_exit vs $got_exit):"
                diff "$on/run.out" "$on/vm.out" | sed 's/^/        /' | head -10
                sed 's/^/        /' "$on/vm.err" | tail -3
                problems="y"
            fi
        fi
    fi

    if [ -z "$problems" ]; then
        echo "ok"; PASS=$((PASS+1))
    else
        echo "FAIL"; FAIL=$((FAIL+1)); FAILED_NAMES+=("$name")
    fi
done

echo
echo "knob matrix: $PASS passed, $FAIL failed (of ${#NAMES[@]})"
if [ "${#REDUCED_NAMES[@]}" -gt 0 ]; then
    echo "  REDUCED to build-only (a runtime dependency was missing, so the"
    echo "  execution half of these fixtures did NOT run): ${REDUCED_NAMES[*]}"
    # Locally this is a warning: not everyone has every fixture's runtime
    # dependency installed, and the build half still gates the codegen. In CI
    # it is fatal. A reduced fixture is still counted a PASS, so without this
    # the job could report green having never executed the behaviour it
    # exists to gate — the silent-skip failure mode TYC_REQUIRE_PYTHON was
    # introduced to close for the interpreter itself. Fatal on ANY cause of
    # reduction, not just a failed install. (PR #360 review, Codex P2.)
    if [ -n "${TYC_REQUIRE_PYTHON:-}" ]; then
        echo "  TYC_REQUIRE_PYTHON is set: reduced coverage is a failure." >&2
        exit 1
    fi
fi
if [ "$FAIL" -gt 0 ]; then
    printf '  failed: %s\n' "${FAILED_NAMES[*]}"
    exit 1
fi
exit 0
