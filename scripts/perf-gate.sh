#!/usr/bin/env bash
#
# perf-gate.sh — build-pipeline performance regression gate (alpha-plan item F2).
#
# Times the full `tyc build` pipeline (preprocess -> parse -> check -> comptime
# -> desugar -> emit -> format) over a fixed, network-free corpus, takes the
# median of N runs, and compares it against the committed baseline in
# perf-baseline.json. Exits non-zero when the median regresses beyond the
# threshold (default 20%).
#
# Usage:
#   scripts/perf-gate.sh             # measure + gate against a same-run control
#   scripts/perf-gate.sh --update    # measure + overwrite the reference baseline
#   scripts/perf-gate.sh --check     # alias for the default (measure + gate)
#   scripts/perf-gate.sh --control-ref REF   # git revision to build the control from
#
# Environment overrides:
#   TYC_BIN          path to the release tyc binary (default: tyc/target/release/tyc)
#   PERF_RUNS        number of timed runs (default: 9)
#   PERF_WARMUP      number of untimed warmup runs per binary (default: 2)
#   PERF_THRESHOLD   regression threshold as a fraction (default: 0.20 = 20%)
#   PERF_BASELINE    path to baseline JSON (default: perf-baseline.json)
#   PERF_CONTROL_BIN a prebuilt control binary (skips the control build)
#   PERF_CONTROL_REF git revision for the control build (default: latest tag)
#
# Why a same-run control. Absolute milliseconds are a property of the machine,
# not the compiler: a 22 ms committed baseline measured on one host reads as
# +140% on another at the same revision. The gate therefore times the
# candidate against a *control built in the same run* — by default the latest
# release tag — interleaved so thermal drift hits both equally, and fails only
# when the candidate is slower than the control by more than the threshold.
# The committed baseline's absolute median is kept for reporting and for hosts
# that have no control available (see the fallback below); it records which
# machine produced it.
#
# Why a shell harness and not Criterion: the existing criterion benches
# (tyc-syntax, tyc-db) measure individual passes in microseconds. This gate
# measures the *whole* CLI build pipeline end-to-end on a realistic multi-file
# project — the number a developer actually feels. It is deliberately
# dependency-free (bash + python3 + jq, all present on CI) and adds no Rust
# build targets, keeping it disjoint from concurrent compiler work.
#
set -euo pipefail

# --- Resolve repo root (this script lives in <root>/scripts) --------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# --- Configuration --------------------------------------------------------
TYC_BIN="${TYC_BIN:-$ROOT/tyc/target/release/tyc}"
PERF_RUNS="${PERF_RUNS:-9}"
PERF_WARMUP="${PERF_WARMUP:-2}"
PERF_THRESHOLD="${PERF_THRESHOLD:-0.20}"
# The same-run control: a prebuilt binary if the caller has one, else built
# from PERF_CONTROL_REF (default: the latest tag). Set PERF_CONTROL_BIN to a
# non-existent path (or PERF_NO_CONTROL=1) to force the absolute-baseline
# fallback.
PERF_CONTROL_BIN="${PERF_CONTROL_BIN:-}"
PERF_CONTROL_REF="${PERF_CONTROL_REF:-}"
PERF_NO_CONTROL="${PERF_NO_CONTROL:-0}"
# Absolute regression floor in milliseconds; see the comparison step. 10 ms
# rather than 5: with a ~22 ms baseline the 20% band is under 5 ms, and a
# shared runner's process-start jitter alone was tripping the gate on a
# tree with no compiler change (2026-09-30 review, §2).
PERF_MIN_SLACK_MS="${PERF_MIN_SLACK_MS:-10}"
PERF_BASELINE="${PERF_BASELINE:-$ROOT/perf-baseline.json}"

# Fixed benchmark corpus: a real, self-contained Typhon project that exercises
# the whole pipeline (classes, async/gather, Result, nullable, multiple
# modules). Built with --no-sync --check so it never touches the network and
# never writes output. To repoint the gate at a different corpus, change this
# one line and refresh the baseline (see perf-baseline.json "methodology").
CORPUS_REL="examples/47-mini-app"
CORPUS="$ROOT/$CORPUS_REL"

# tyc build flags: --no-sync skips `uv sync` (no network); --check runs the
# full pipeline as a dry run without writing files.
BUILD_FLAGS=(build "$CORPUS" --no-sync --check --no-format)

# Keep the measurement on *compiler* work.
#
# The gate's job is to catch a regression in the Rust pipeline, and its 20%
# threshold is meaningful only in proportion to how much of the measured time
# the pipeline actually accounts for. Venv introspection spawns a Python
# subprocess and imports the project's dependencies; that was 55-59% of the
# measured wall-clock, so a 20% nominal threshold tolerated a ~45% regression
# in the code under test, and every measurement inherited the variance of a
# Python interpreter start. `ruff format` shells out for the same reason.
#
# Both are excluded here (`--no-format` above, `TYC_NO_INTROSPECT` below), so
# the number means what the threshold claims. Re-baseline when changing either
# (see perf-baseline.json "methodology").
export TYC_NO_INTROSPECT=1

MODE="check"
while [ $# -gt 0 ]; do
  case "$1" in
    --update) MODE="update"; shift ;;
    --check) MODE="check"; shift ;;
    --control-ref) PERF_CONTROL_REF="$2"; shift 2 ;;
    --no-control) PERF_NO_CONTROL=1; shift ;;
    -h | --help)
      sed -n '2,40p' "${BASH_SOURCE[0]}"
      exit 0
      ;;
    *)
      echo "perf-gate.sh: unknown argument '$1' (use --check, --update, --control-ref REF or --no-control)" >&2
      exit 2
      ;;
  esac
done

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/tyc-perf.XXXXXX")"
cleanup() { rm -rf "$SCRATCH"; }
trap cleanup EXIT

# --- Preflight ------------------------------------------------------------
if [[ ! -x "$TYC_BIN" ]]; then
  echo "perf-gate.sh: release binary not found at $TYC_BIN" >&2
  echo "  build it first: (cd tyc && cargo build --release --bin tyc)" >&2
  exit 2
fi
if [[ ! -d "$CORPUS" ]]; then
  echo "perf-gate.sh: benchmark corpus not found at $CORPUS" >&2
  exit 2
fi
for tool in python3 jq; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "perf-gate.sh: required tool '$tool' not on PATH" >&2
    exit 2
  }
done

echo "perf-gate: corpus=$CORPUS_REL runs=$PERF_RUNS warmup=$PERF_WARMUP threshold=$(python3 -c "print(f'{$PERF_THRESHOLD*100:.0f}%')")"

# --- Sanity: the corpus must build cleanly, else timings are meaningless ---
if ! "$TYC_BIN" "${BUILD_FLAGS[@]}" >/dev/null 2>"$ROOT/.perf-gate-stderr.log"; then
  echo "perf-gate: corpus build FAILED — cannot benchmark. tyc output:" >&2
  cat "$ROOT/.perf-gate-stderr.log" >&2
  rm -f "$ROOT/.perf-gate-stderr.log"
  exit 2
fi
rm -f "$ROOT/.perf-gate-stderr.log"

# --- Same-run control -----------------------------------------------------
# A control binary lets the gate compare like with like on any host. Prefer a
# caller-supplied one; otherwise build the reference revision into a scratch
# target dir so the working tree's `tyc/target` is untouched.
CONTROL_BIN=""
CONTROL_DESC=""
if [[ "$PERF_NO_CONTROL" != "1" ]]; then
  if [[ -n "$PERF_CONTROL_BIN" && -x "$PERF_CONTROL_BIN" ]]; then
    CONTROL_BIN="$PERF_CONTROL_BIN"
    CONTROL_DESC="supplied $PERF_CONTROL_BIN"
  elif [[ "$MODE" != "update" ]]; then
    ref="$PERF_CONTROL_REF"
    if [[ -z "$ref" ]]; then
      ref="$(git -C "$ROOT" describe --tags --abbrev=0 2>/dev/null || true)"
    fi
    if [[ -n "$ref" ]]; then
      echo "perf-gate: building control from $ref into $SCRATCH/target (one-off)…"
      control_src="$SCRATCH/control-src"
      if git -C "$ROOT" worktree add --detach "$control_src" "$ref" >/dev/null 2>&1; then
        if CARGO_TARGET_DIR="$SCRATCH/target" \
             bash -c "cd '$control_src/tyc' && cargo build --release --bin tyc" >/dev/null 2>&1; then
          CONTROL_BIN="$SCRATCH/target/release/tyc"
          CONTROL_DESC="built from $ref"
        fi
        git -C "$ROOT" worktree remove --force "$control_src" >/dev/null 2>&1 || true
      fi
    fi
  fi
fi
if [[ -n "$CONTROL_BIN" ]]; then
  echo "perf-gate: control=$CONTROL_DESC"
else
  echo "perf-gate: control=unavailable — falling back to the absolute baseline"
fi

# --- Warmup (fills OS/page caches; not timed) -----------------------------
for ((i = 0; i < PERF_WARMUP; i++)); do
  "$TYC_BIN" "${BUILD_FLAGS[@]}" >/dev/null 2>&1
  [[ -n "$CONTROL_BIN" ]] && "$CONTROL_BIN" "${BUILD_FLAGS[@]}" >/dev/null 2>&1
done

# --- Timed runs -----------------------------------------------------------
# Interleave candidate and control so thermal drift and page-cache state hit
# both equally. Time the run in Python: `date +%s%N` is a GNU extension that
# emits a literal `%N` on macOS/BSD `date`. `time.perf_counter()` is portable.
time_run() {
  python3 -c 'import time, subprocess, sys
t0 = time.perf_counter()
subprocess.run([sys.argv[1], *sys.argv[2:]], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
print(int((time.perf_counter() - t0) * 1000))' "$1" "${BUILD_FLAGS[@]}"
}

samples=()
control_samples=()
for ((i = 0; i < PERF_RUNS; i++)); do
  samples+=("$(time_run "$TYC_BIN")")
  if [[ -n "$CONTROL_BIN" ]]; then
    control_samples+=("$(time_run "$CONTROL_BIN")")
  fi
done

echo "perf-gate: samples (ms) = ${samples[*]}"
[[ -n "$CONTROL_BIN" ]] && echo "perf-gate: control samples (ms) = ${control_samples[*]}"

# --- Median (python3: robust integer median, plus min/max for reporting) --
read -r MEDIAN MIN MAX <<<"$(python3 - "${samples[@]}" <<'PY'
import sys, statistics
xs = sorted(int(x) for x in sys.argv[1:])
print(int(statistics.median(xs)), min(xs), max(xs))
PY
)"
echo "perf-gate: measured median=${MEDIAN}ms (min=${MIN}ms max=${MAX}ms)"

CONTROL_MEDIAN=""
if [[ -n "$CONTROL_BIN" ]]; then
  CONTROL_MEDIAN="$(python3 - "${control_samples[@]}" <<'PY'
import sys, statistics
xs = sorted(int(x) for x in sys.argv[1:])
print(int(statistics.median(xs)))
PY
)"
  echo "perf-gate: control median=${CONTROL_MEDIAN}ms"
fi

# --- Update mode: write the baseline and exit -----------------------------
if [[ "$MODE" == "update" ]]; then
  # Record the host, so a reader knows which machine the absolute median
  # belongs to. The same-run control is the cross-host verdict; this is for
  # reporting and the no-control fallback.
  HOST_DESC="$(uname -sm 2>/dev/null || echo unknown)"
  HOST_DESC="$HOST_DESC / $(sysctl -n machdep.cpu.brand_string 2>/dev/null \
    || grep -m1 'model name' /proc/cpuinfo 2>/dev/null | sed 's/.*: //' \
    || echo 'unknown cpu')"
  tmp="$(mktemp)"
  if [[ -f "$PERF_BASELINE" ]]; then
    jq \
      --argjson median "$MEDIAN" \
      --arg corpus "$CORPUS_REL" \
      --argjson runs "$PERF_RUNS" \
      --argjson threshold "$PERF_THRESHOLD" \
      --arg date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      --arg host "$HOST_DESC" \
      '.median_ms = $median
       | .corpus = $corpus
       | .runs = $runs
       | .threshold = $threshold
       | .recorded_utc = $date
       | .recorded_host = $host' \
      "$PERF_BASELINE" >"$tmp"
  else
    cat >"$tmp" <<EOF
{
  "median_ms": $MEDIAN,
  "corpus": "$CORPUS_REL",
  "runs": $PERF_RUNS,
  "threshold": $PERF_THRESHOLD,
  "recorded_utc": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "recorded_host": "$HOST_DESC"
}
EOF
  fi
  mv "$tmp" "$PERF_BASELINE"
  echo "perf-gate: baseline updated -> $PERF_BASELINE (median_ms=$MEDIAN on $HOST_DESC)"
  exit 0
fi

# --- Check mode: compare against committed baseline -----------------------
if [[ ! -f "$PERF_BASELINE" ]]; then
  echo "perf-gate: no baseline at $PERF_BASELINE — run 'scripts/perf-gate.sh --update' first" >&2
  exit 2
fi

BASELINE_MS="$(jq -r '.median_ms' "$PERF_BASELINE")"

# Verdict. With a same-run control the comparison is candidate-vs-control on
# this host, which is the only number that isolates the compiler change; the
# committed absolute median is then reporting only. Without a control, fall
# back to the absolute baseline plus an absolute slack floor — and say so,
# because on a host other than the recording one that verdict is a property of
# the machine, not the tree.
python3 - "$MEDIAN" "$BASELINE_MS" "$PERF_THRESHOLD" "$PERF_MIN_SLACK_MS" \
         "${CONTROL_MEDIAN:-}" <<'PY'
import sys
median = float(sys.argv[1])
baseline = float(sys.argv[2])
threshold = float(sys.argv[3])
min_slack = float(sys.argv[4])
control = float(sys.argv[5]) if sys.argv[5] else None

if control is not None:
    ratio = median / control if control else 0.0
    delta = (ratio - 1.0) * 100.0
    print(f"perf-gate: candidate={median:.0f}ms  control={control:.0f}ms  "
          f"delta={delta:+.1f}%  (committed baseline {baseline:.0f}ms, reporting only)")
    if ratio > 1.0 + threshold:
        print(f"perf-gate: FAIL — candidate is {delta:+.1f}% slower than the "
              f"same-run control (> +{threshold*100:.0f}%).")
        print("perf-gate: this is a real regression in the tree, not host drift. "
              "Fix it, or if intentional refresh the reference with "
              "'scripts/perf-gate.sh --update'.")
        sys.exit(1)
    print("perf-gate: PASS — within threshold of the same-run control.")
else:
    # Absolute-baseline fallback (no control could be built or supplied).
    limit = max(baseline * (1.0 + threshold), baseline + min_slack)
    delta = (median - baseline) / baseline * 100.0 if baseline else 0.0
    print(f"perf-gate: baseline={baseline:.0f}ms  measured={median:.0f}ms  "
          f"delta={delta:+.1f}%  limit={limit:.0f}ms "
          f"(+{threshold*100:.0f}%, min slack {min_slack:.0f}ms)")
    if median > limit:
        print(f"perf-gate: FAIL — build pipeline regressed {delta:+.1f}% "
              f"(> +{threshold*100:.0f}% threshold).")
        print("perf-gate: NOTE this compares against an absolute baseline recorded "
              "on a different machine; if the tree is unchanged, treat it as host "
              "drift and build a control (--control-ref REF) instead.")
        print("perf-gate: if this is an intentional, justified change, refresh the "
              "baseline with 'scripts/perf-gate.sh --update' and commit "
              "perf-baseline.json.")
        sys.exit(1)
    print("perf-gate: PASS — within threshold.")
PY
