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
#   scripts/perf-gate.sh             # measure + gate against committed baseline
#   scripts/perf-gate.sh --update    # measure + overwrite the baseline median
#   scripts/perf-gate.sh --check     # alias for the default (measure + gate)
#
# Environment overrides:
#   TYC_BIN          path to the release tyc binary (default: tyc/target/release/tyc)
#   PERF_RUNS        number of timed runs (default: 9)
#   PERF_WARMUP      number of untimed warmup runs (default: 2)
#   PERF_THRESHOLD   regression threshold as a fraction (default: 0.20 = 20%)
#   PERF_BASELINE    path to baseline JSON (default: perf-baseline.json)
#
# Why a shell harness and not Criterion: the existing criterion benches
# (tyc-syntax, tyc-db) measure individual passes in microseconds. This gate
# measures the *whole* CLI build pipeline end-to-end on a realistic multi-file
# project — the number a developer actually feels — and produces a single
# median that CI can compare against a committed value. It is deliberately
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
BUILD_FLAGS=(build "$CORPUS" --no-sync --check)

MODE="check"
case "${1:-}" in
  --update) MODE="update" ;;
  --check | "") MODE="check" ;;
  -h | --help)
    sed -n '2,33p' "${BASH_SOURCE[0]}"
    exit 0
    ;;
  *)
    echo "perf-gate.sh: unknown argument '$1' (use --check or --update)" >&2
    exit 2
    ;;
esac

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

# --- Warmup (fills OS/page caches; not timed) -----------------------------
for ((i = 0; i < PERF_WARMUP; i++)); do
  "$TYC_BIN" "${BUILD_FLAGS[@]}" >/dev/null 2>&1
done

# --- Timed runs -----------------------------------------------------------
samples=()
for ((i = 0; i < PERF_RUNS; i++)); do
  start=$(date +%s%N)
  "$TYC_BIN" "${BUILD_FLAGS[@]}" >/dev/null 2>&1
  end=$(date +%s%N)
  ms=$(((end - start) / 1000000))
  samples+=("$ms")
done

echo "perf-gate: samples (ms) = ${samples[*]}"

# --- Median (python3: robust integer median, plus min/max for reporting) --
read -r MEDIAN MIN MAX <<<"$(python3 - "${samples[@]}" <<'PY'
import sys, statistics
xs = sorted(int(x) for x in sys.argv[1:])
print(int(statistics.median(xs)), min(xs), max(xs))
PY
)"
echo "perf-gate: measured median=${MEDIAN}ms (min=${MIN}ms max=${MAX}ms)"

# --- Update mode: write the baseline and exit -----------------------------
if [[ "$MODE" == "update" ]]; then
  tmp="$(mktemp)"
  if [[ -f "$PERF_BASELINE" ]]; then
    jq \
      --argjson median "$MEDIAN" \
      --arg corpus "$CORPUS_REL" \
      --argjson runs "$PERF_RUNS" \
      --argjson threshold "$PERF_THRESHOLD" \
      --arg date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      '.median_ms = $median
       | .corpus = $corpus
       | .runs = $runs
       | .threshold = $threshold
       | .recorded_utc = $date' \
      "$PERF_BASELINE" >"$tmp"
  else
    cat >"$tmp" <<EOF
{
  "median_ms": $MEDIAN,
  "corpus": "$CORPUS_REL",
  "runs": $PERF_RUNS,
  "threshold": $PERF_THRESHOLD,
  "recorded_utc": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
  fi
  mv "$tmp" "$PERF_BASELINE"
  echo "perf-gate: baseline updated -> $PERF_BASELINE (median_ms=$MEDIAN)"
  exit 0
fi

# --- Check mode: compare against committed baseline -----------------------
if [[ ! -f "$PERF_BASELINE" ]]; then
  echo "perf-gate: no baseline at $PERF_BASELINE — run 'scripts/perf-gate.sh --update' first" >&2
  exit 2
fi

BASELINE_MS="$(jq -r '.median_ms' "$PERF_BASELINE")"

# Compute limit = baseline * (1 + threshold), the verdict, and a readable
# report in python3 (no float math in bash).
python3 - "$MEDIAN" "$BASELINE_MS" "$PERF_THRESHOLD" <<'PY'
import sys
median = float(sys.argv[1])
baseline = float(sys.argv[2])
threshold = float(sys.argv[3])
limit = baseline * (1.0 + threshold)
delta = (median - baseline) / baseline * 100.0 if baseline else 0.0
print(f"perf-gate: baseline={baseline:.0f}ms  measured={median:.0f}ms  "
      f"delta={delta:+.1f}%  limit={limit:.0f}ms (+{threshold*100:.0f}%)")
if median > limit:
    print(f"perf-gate: FAIL — build pipeline regressed {delta:+.1f}% "
          f"(> +{threshold*100:.0f}% threshold).")
    print("perf-gate: if this is an intentional, justified change, refresh the "
          "baseline with 'scripts/perf-gate.sh --update' and commit "
          "perf-baseline.json.")
    sys.exit(1)
print("perf-gate: PASS — within threshold.")
PY
