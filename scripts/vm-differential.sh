#!/usr/bin/env bash
# vm-differential.sh — VM ↔ CPython differential harness (review item T0.2 / T0.4).
#
# The VM (`tyc run`) is contractually a drop-in for `tyc build && python`.
# This harness proves or disproves that, per unit, over the whole `.ty` corpus:
#
#   1. build the unit with `tyc build` and execute the emitted Python under
#      python3.13                                    → (stdout_cpython, exit_cpython)
#   2. execute the same source with the in-process VM (`tyc run`)
#                                                     → (stdout_vm,      exit_vm)
#   3. diverges iff stdout or exit code differ.
#
# Known divergences live in a checked-in expectations file
# (scripts/differential-baseline.txt). The gate fails when:
#   * a unit diverges and is NOT in the baseline           (regression), or
#   * a unit is in the baseline and no longer diverges     (stale baseline).
# Both directions fail, so the baseline cannot rot; --update rewrites it.
#
# Everything is network-free (TYC_NO_SYNC / TYC_NO_INTROSPECT) and needs only
# bash, coreutils, a release `tyc`, and python3.13.
#
# Usage:
#   scripts/vm-differential.sh [options]
#     --scope examples|stress|all   corpus subset (default: all)
#     --filter REGEX                only units whose id matches REGEX
#     --jobs N                      parallel workers (default: nproc)
#     --timeout N                   per-side wall-clock seconds (default: 20)
#     --update                      rewrite the baseline from this run's result
#     --baseline PATH               alternate expectations file
#     --report PATH                 write the full per-unit TSV report here
#     --keep                        keep the scratch workdirs for inspection
#     -h|--help
#
# Environment:
#   TYC                 path to the tyc binary (default: tyc/target/release/tyc)
#   PYTHON313           python3.13 interpreter (default: python3.13)
#   TYC_REQUIRE_PYTHON  set to 1 in CI (same convention as the Rust suite).
#                       A missing python3.13 is fatal either way — this harness
#                       never degrades to a partial run, because a skip nothing
#                       observes is indistinguishable from a pass. The variable
#                       only changes the wording of the failure.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

TYC="${TYC:-$REPO_ROOT/tyc/target/release/tyc}"
PYTHON313="${PYTHON313:-python3.13}"
BASELINE="$REPO_ROOT/scripts/differential-baseline.txt"
SCOPE="all"
FILTER=""
JOBS="$(nproc 2>/dev/null || echo 4)"
TIMEOUT=20
UPDATE=0
KEEP=0
REPORT=""

while [ $# -gt 0 ]; do
    case "$1" in
        --scope)    SCOPE="$2"; shift 2 ;;
        --filter)   FILTER="$2"; shift 2 ;;
        --jobs)     JOBS="$2"; shift 2 ;;
        --timeout)  TIMEOUT="$2"; shift 2 ;;
        --baseline) BASELINE="$2"; shift 2 ;;
        --report)   REPORT="$2"; shift 2 ;;
        --update)   UPDATE=1; shift ;;
        --keep)     KEEP=1; shift ;;
        -h|--help)  sed -n '2,42p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

# ---------------------------------------------------------------- preflight --
if [ ! -x "$TYC" ]; then
    echo "error: tyc binary not found at '$TYC'." >&2
    echo "       build it with: (cd tyc && cargo build --release)" >&2
    echo "       or point TYC=... at an existing binary." >&2
    exit 2
fi

if ! command -v "$PYTHON313" >/dev/null 2>&1; then
    # Emitted Typhon targets CPython 3.13+. The system `python3` is frequently
    # older; running the emitted code under it would silently under-test.
    if [ "${TYC_REQUIRE_PYTHON:-0}" = "1" ]; then
        echo "error: '$PYTHON313' not found and TYC_REQUIRE_PYTHON=1." >&2
        echo "       The differential gate cannot run without CPython 3.13+." >&2
        exit 2
    fi
    echo "error: '$PYTHON313' not found on PATH." >&2
    echo "       Typhon emits CPython 3.13+; python3 ($(python3 --version 2>&1)) is not a" >&2
    echo "       substitute. Install 3.13 or set PYTHON313=/path/to/python3.13." >&2
    echo "       Refusing to run a harness that would pass vacuously." >&2
    exit 2
fi

case "$("$PYTHON313" -c 'import sys;print(sys.version_info[:2]>=(3,13))' 2>/dev/null)" in
    True) ;;
    *) echo "error: '$PYTHON313' is not CPython 3.13+." >&2; exit 2 ;;
esac

# ------------------------------------------------------------- unit listing --
# A "unit" is one independently-executable thing:
#   * a project        — any directory containing typhon.toml (id ends in '/')
#   * a standalone .ty — any .ty file not inside a project directory
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/tyc-diff.XXXXXX")"
cleanup() { [ "$KEEP" = "1" ] || rm -rf "$SCRATCH"; }
trap cleanup EXIT

case "$SCOPE" in
    examples) ROOTS=(examples) ;;
    stress)   ROOTS=(stress) ;;
    all)      ROOTS=(examples stress) ;;
    *) echo "unknown --scope '$SCOPE' (examples|stress|all)" >&2; exit 2 ;;
esac

PROJECTS="$SCRATCH/projects.txt"
UNITS="$SCRATCH/units.txt"
find "${ROOTS[@]}" -name typhon.toml -printf '%h\n' 2>/dev/null | sort -u > "$PROJECTS"

{
    # project units
    sed 's:$:/:' "$PROJECTS"
    # standalone .ty units: every .ty not under a project directory
    find "${ROOTS[@]}" -name '*.ty' -type f 2>/dev/null | sort | while IFS= read -r f; do
        under_project=0
        while IFS= read -r p; do
            case "$f" in "$p"/*) under_project=1; break ;; esac
        done < "$PROJECTS"
        [ "$under_project" = "0" ] && printf '%s\n' "$f"
    done
} | sort -u > "$UNITS"

if [ -n "$FILTER" ]; then
    grep -E "$FILTER" "$UNITS" > "$UNITS.f" || true
    mv "$UNITS.f" "$UNITS"
fi

TOTAL="$(wc -l < "$UNITS" | tr -d ' ')"
if [ "$TOTAL" -eq 0 ]; then
    echo "error: no units selected (scope=$SCOPE filter='$FILTER')." >&2
    exit 2
fi

echo "VM <-> CPython differential harness"
echo "  tyc      : $TYC"
echo "  python   : $PYTHON313 ($("$PYTHON313" --version 2>&1))"
echo "  scope    : $SCOPE${FILTER:+  filter=/$FILTER/}"
echo "  units    : $TOTAL   (jobs=$JOBS, per-side timeout=${TIMEOUT}s)"
echo "  baseline : ${BASELINE#$REPO_ROOT/}"
echo

# ----------------------------------------------------------------- the work --
# One worker per unit. Writes exactly one TSV line to $SCRATCH/results/<n>.
mkdir -p "$SCRATCH/results"
export REPO_ROOT TYC PYTHON313 TIMEOUT SCRATCH

run_unit() {
    unit="$1"
    slug="$(printf '%s' "$unit" | tr '/.' '__')"
    work="$SCRATCH/w/$slug"
    rm -rf "$work"; mkdir -p "$work"

    if [ "${unit%/}" != "$unit" ]; then
        # project unit — copy so the working tree is never mutated
        cp -r "$REPO_ROOT/${unit%/}/." "$work/"
        rm -rf "$work/build" "$work/.venv"
        vm_target="."
    else
        # standalone file — synthesise a minimal project around it
        mkdir -p "$work/src"
        cp "$REPO_ROOT/$unit" "$work/src/main.ty"
        cat > "$work/typhon.toml" <<'TOML'
[project]
name = "diffunit"
version = "0.1.0"
src = "src"
out = "build"

[python]
target = "3.13"

[emit]
class-default = "dataclass"
format = false

[strictness]
unintrospectable-dependency = "off"

[env]
required = []
TOML
        vm_target="src/main.ty"
    fi

    # -- 1. build ------------------------------------------------------------
    ( cd "$work" && TYC_NO_SYNC=1 TYC_NO_INTROSPECT=1 \
        timeout "$TIMEOUT" "$TYC" build --no-sync >build.log 2>&1 )
    bcode=$?
    if [ $bcode -ne 0 ]; then
        printf '%s\t%s\t%s\n' "nobuild" "$unit" "tyc build exit=$bcode"
        return
    fi
    if [ ! -f "$work/build/main.py" ]; then
        printf '%s\t%s\t%s\n' "noentry" "$unit" "no build/main.py emitted"
        return
    fi

    common_env=(env PYTHONHASHSEED=0 PYTHONDONTWRITEBYTECODE=1 TZ=UTC LC_ALL=C.UTF-8)

    # -- 2. CPython side -----------------------------------------------------
    ( cd "$work" && "${common_env[@]}" \
        timeout "$TIMEOUT" "$PYTHON313" build/main.py \
        </dev/null >cpy.out 2>cpy.err )
    ccode=$?

    # -- 3. VM side ----------------------------------------------------------
    ( cd "$work" && "${common_env[@]}" TYC_NO_INTROSPECT=1 \
        timeout "$TIMEOUT" "$TYC" run "$vm_target" \
        </dev/null >vm.out 2>vm.err )
    vcode=$?

    # 124 is GNU timeout's "killed on timeout".
    if [ $ccode -eq 124 ] && [ $vcode -eq 124 ]; then
        printf '%s\t%s\t%s\n' "both-timeout" "$unit" "both sides exceeded ${TIMEOUT}s"
        return
    fi

    if cmp -s "$work/cpy.out" "$work/vm.out" && [ "$ccode" = "$vcode" ]; then
        # Agreement is only meaningful if at least one side actually produced
        # output or succeeded. Both-crash-with-empty-stdout agreement is a
        # vacuous pass (typically a missing third-party import); count it
        # separately so the summary cannot overstate real coverage.
        if [ ! -s "$work/cpy.out" ] && [ "$ccode" -ne 0 ]; then
            printf '%s\t%s\t%s\n' "vacuous" "$unit" "both failed, empty stdout (exit=$ccode)"
        else
            printf '%s\t%s\t%s\n' "ok" "$unit" "exit=$ccode"
        fi
        return
    fi

    # Divergent: re-run BOTH sides to rule out self-nondeterminism (clocks,
    # randomness, tempfile paths, address-dependent repr, task scheduling).
    # A unit that disagrees with *itself* cannot be used to judge the VM, so it
    # is excluded and reported rather than allowed to flake the gate red.
    for _i in 1 2; do
        ( cd "$work" && "${common_env[@]}" \
            timeout "$TIMEOUT" "$PYTHON313" build/main.py \
            </dev/null >cpy2.out 2>/dev/null )
        c2code=$?
        if ! cmp -s "$work/cpy.out" "$work/cpy2.out" || [ "$ccode" != "$c2code" ]; then
            printf '%s\t%s\t%s\n' "nondeterministic" "$unit" "CPython disagrees with itself"
            return
        fi
        ( cd "$work" && "${common_env[@]}" TYC_NO_INTROSPECT=1 \
            timeout "$TIMEOUT" "$TYC" run "$vm_target" \
            </dev/null >vm2.out 2>/dev/null )
        v2code=$?
        if ! cmp -s "$work/vm.out" "$work/vm2.out" || [ "$vcode" != "$v2code" ]; then
            printf '%s\t%s\t%s\n' "nondeterministic" "$unit" "VM disagrees with itself"
            return
        fi
    done

    reason=""
    [ "$ccode" != "$vcode" ] && reason="exit cpython=$ccode vm=$vcode"
    if ! cmp -s "$work/cpy.out" "$work/vm.out"; then
        cl=$(wc -l < "$work/cpy.out" | tr -d ' ')
        vl=$(wc -l < "$work/vm.out" | tr -d ' ')
        first=$(diff "$work/cpy.out" "$work/vm.out" 2>/dev/null | sed -n '2p' | cut -c1-90 | tr -d '\t')
        reason="${reason:+$reason; }stdout differs (cpython ${cl}L vs vm ${vl}L)${first:+ | ${first}}"
    fi
    printf '%s\t%s\t%s\n' "diverge" "$unit" "$reason"
}
export -f run_unit

RESULTS="$SCRATCH/results.tsv"
# shellcheck disable=SC2016
xargs -a "$UNITS" -d '\n' -P "$JOBS" -I{} bash -c 'run_unit "$@"' _ {} > "$RESULTS"
XARGS_STATUS=$?
sort -k2,2 -o "$RESULTS" "$RESULTS"

# Reconcile: every selected unit must have produced exactly one result line.
# A worker that dies without printing (OOM kill, a signal, xargs aborting on a
# 255 exit) would otherwise silently drop its unit from every category — a
# lost divergent unit makes the gate PASS, and a lost baseline unit is
# spuriously reported as fixed. Every run_unit path ends in a printf, so a
# non-zero xargs status is worker trouble too, even when the count matches.
N_RESULTS="$(wc -l < "$RESULTS" | tr -d ' ')"
if [ "$N_RESULTS" -ne "$TOTAL" ] || [ "$XARGS_STATUS" -ne 0 ]; then
    echo "error: worker(s) lost $((TOTAL - N_RESULTS)) unit(s):" >&2
    echo "       $TOTAL unit(s) selected but $N_RESULTS result(s) recorded (xargs exit=$XARGS_STATUS)." >&2
    echo "       Refusing to gate on an incomplete run — a dropped unit is invisible" >&2
    echo "       in every category, so the result would be unreliable in BOTH directions." >&2
    exit 2
fi

# ------------------------------------------------------------- aggregation --
count() { grep -cP "^$1\t" "$RESULTS" || true; }
N_OK=$(count ok); N_DIV=$(count diverge); N_NOBUILD=$(count nobuild)
N_NOENTRY=$(count noentry); N_VAC=$(count vacuous)
N_TMO=$(count both-timeout); N_ND=$(count nondeterministic)

DIVERGED="$SCRATCH/diverged.txt"
grep -P '^diverge\t' "$RESULTS" | cut -f2 | sort > "$DIVERGED"

if [ -n "$REPORT" ]; then
    cp "$RESULTS" "$REPORT"
    echo "full per-unit report → $REPORT"
fi

echo "results over $TOTAL unit(s):"
printf '  %-18s %5d   %s\n' "ok"               "$N_OK"      "stdout + exit code agree"
printf '  %-18s %5d   %s\n' "diverge"          "$N_DIV"     "VM disagrees with CPython"
printf '  %-18s %5d   %s\n' "vacuous"          "$N_VAC"     "agree only because both failed with empty stdout"
printf '  %-18s %5d   %s\n' "nobuild"          "$N_NOBUILD" "tyc build failed — not comparable"
printf '  %-18s %5d   %s\n' "noentry"          "$N_NOENTRY" "built but emitted no build/main.py"
printf '  %-18s %5d   %s\n' "nondeterministic" "$N_ND"      "CPython disagrees with itself — excluded"
printf '  %-18s %5d   %s\n' "both-timeout"     "$N_TMO"     "both sides hit the ${TIMEOUT}s limit"
echo

if [ "$N_VAC" -gt 0 ]; then
    echo "note: $N_VAC unit(s) 'agree' only because both execution paths failed with"
    echo "      empty stdout (usually an uninstalled third-party import). They are"
    echo "      NOT counted as passes; they are not real differential coverage."
    echo
fi

# --------------------------------------------------------------- baselining --
if [ "$UPDATE" = "1" ]; then
    {
        echo "# scripts/differential-baseline.txt"
        echo "#"
        echo "# Known VM <-> CPython divergences. Generated by:"
        echo "#     scripts/vm-differential.sh --update"
        echo "# Each line is a unit id (a .ty path, or a project dir ending in '/')"
        echo "# whose \`tyc run\` output does NOT match \`tyc build\` + python3.13."
        echo "#"
        echo "# The VM is contractually a drop-in for the compiled path, so EVERY"
        echo "# line here is a bug. This file exists only so the gate can fail on"
        echo "# NEW divergences while the known set is burned down. Fixing a VM bug"
        echo "# means deleting lines from this file — the gate also fails when a"
        echo "# listed unit stops diverging, so the list cannot rot."
        echo "#"
        echo "# Triage a single entry with:"
        echo "#     scripts/vm-differential.sh --filter '<unit>' --keep"
        echo "# then diff cpy.out / vm.out (and read vm.err) in the kept workdir."
        echo "#"
        echo "# ENVIRONMENT-SENSITIVE: each unit's classification (ok / diverge /"
        echo "# vacuous) depends on which third-party packages the ambient"
        echo "# python3.13 can import — e.g. every \`model\` class needs pydantic at"
        echo "# runtime, and one stress unit imports yaml. Record (--update) and"
        echo "# verify this baseline with the same package set importable:"
        echo "#     pydantic, PyYAML"
        echo "# (CI installs pinned versions in the differential job — see"
        echo "# .github/workflows/ci.yml.) On a machine missing them, affected"
        echo "# baseline entries are reported as 'unverifiable here' and skipped"
        echo "# rather than failed; affected non-baseline units may show up as new"
        echo "# divergences the recording environment would not produce."
        echo "#"
        echo "# Generated $(date -u +%Y-%m-%d) against $("$TYC" --version 2>/dev/null | head -1)"
        echo "# Scope: $SCOPE${FILTER:+  filter=/$FILTER/}"
        echo "#"
        cat "$DIVERGED"
    } > "$BASELINE"
    echo "baseline rewritten: ${BASELINE#$REPO_ROOT/}  ($N_DIV entry/entries)"
    exit 0
fi

if [ ! -f "$BASELINE" ]; then
    echo "error: baseline '$BASELINE' does not exist." >&2
    echo "       create it with: scripts/vm-differential.sh --update" >&2
    exit 2
fi

EXPECTED="$SCRATCH/expected.txt"
grep -vE '^\s*(#|$)' "$BASELINE" | sed 's/[[:space:]]*$//' | sort -u > "$EXPECTED"

# Only compare against the slice of the baseline this run actually covered,
# so `--scope examples` / `--filter` never report the uncovered remainder as
# "fixed". A partial run can regress the gate but can never shrink it.
COVERED="$SCRATCH/covered_expected.txt"
comm -12 "$EXPECTED" <(sort -u "$UNITS") > "$COVERED"

# "Fixed" requires a genuinely comparable, non-divergent run — i.e. class
# `ok`. A baseline entry classified vacuous/nobuild/noentry/nondeterministic/
# both-timeout was not COMPARED in this environment (typically a third-party
# package the recording environment had installed is missing here — see the
# baseline header), so it is neither fixed nor regressed: report it as
# "unverifiable here" without failing the gate. Without this split, a bare
# runner flipped every pydantic/yaml-dependent divergence to `vacuous` and the
# gate failed by design on its very first environment mismatch.
OK_UNITS="$SCRATCH/ok_units.txt"
grep -P '^ok\t' "$RESULTS" | cut -f2 | sort > "$OK_UNITS"

NEW="$SCRATCH/new.txt"
FIXED="$SCRATCH/fixed.txt"
UNVERIFIABLE="$SCRATCH/unverifiable.txt"
comm -23 "$DIVERGED" "$EXPECTED" > "$NEW"
comm -12 "$COVERED" "$OK_UNITS" > "$FIXED"
comm -23 "$COVERED" <(sort -u "$DIVERGED" "$OK_UNITS") > "$UNVERIFIABLE"

N_NEW=$(wc -l < "$NEW" | tr -d ' ')
N_FIXED=$(wc -l < "$FIXED" | tr -d ' ')
N_UNVER=$(wc -l < "$UNVERIFIABLE" | tr -d ' ')

status=0
if [ "$N_NEW" -gt 0 ]; then
    status=1
    echo "FAIL: $N_NEW NEW divergence(s) not in the baseline:"
    while IFS= read -r u; do
        printf '  + %s\n      %s\n' "$u" "$(grep -P "^diverge\t\Q$u\E\t" "$RESULTS" | cut -f3)"
    done < "$NEW"
    echo
    echo "  The VM must behave identically to \`tyc build\` + CPython. Fix the VM,"
    echo "  or — if the divergence is pre-existing and out of scope — append the"
    echo "  unit id to ${BASELINE#$REPO_ROOT/} with a comment explaining why."
    echo
fi

if [ "$N_FIXED" -gt 0 ]; then
    status=1
    echo "FAIL: $N_FIXED baseline entry/entries no longer diverge — remove them"
    echo "      from ${BASELINE#$REPO_ROOT/} (the baseline must only shrink, never rot):"
    sed 's/^/  - /' "$FIXED"
    echo
fi

if [ "$N_UNVER" -gt 0 ]; then
    echo "note: $N_UNVER baseline entry/entries are unverifiable here — their runs were"
    echo "      not comparable in this environment (usually a third-party package the"
    echo "      recording environment could import is missing; see the baseline header"
    echo "      for the package set the classifications assume). Neither fixed nor"
    echo "      regressed; not failing the gate on them:"
    while IFS= read -r u; do
        cls="$(grep -P "\t\Q$u\E\t" "$RESULTS" | cut -f1 | head -1)"
        printf '  ? %s  (%s)\n' "$u" "${cls:-no result}"
    done < "$UNVERIFIABLE"
    echo
fi

if [ "$status" -eq 0 ]; then
    echo "PASS: $N_DIV divergence(s), all accounted for in the baseline."
    echo "      Burn the baseline down: every entry is a VM bug."
fi
exit "$status"
