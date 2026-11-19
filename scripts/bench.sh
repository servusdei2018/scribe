#!/usr/bin/env bash
# Benchmark scribe vs. sphinx-build on an identical, offline multi-page fixture.
#
# Measures wall-clock time and peak RSS (via GNU time -v) for cold `-E` HTML
# builds. Runs a warmup pass, then RUNS measured passes per engine, and prints
# medians plus the measured scribe/sphinx-build ratio. No numbers are
# hard-coded; everything printed is measured on the current machine.
#
# Usage:
#   SCRIBE_BIN=./target/release/scribe \
#   SPHINX_BUILD=.oracle-venv/bin/sphinx-build \
#   RUNS=5 PAGES=40 ./scripts/bench.sh [results.md]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIBE_BIN="${SCRIBE_BIN:-$ROOT/target/release/scribe}"
SPHINX_BUILD="${SPHINX_BUILD:-$ROOT/.oracle-venv/bin/sphinx-build}"
PYTHON="${PYTHON:-$ROOT/.oracle-venv/bin/python}"
RUNS="${RUNS:-5}"
PAGES="${PAGES:-40}"
TIME="${TIME:-/usr/bin/time}"
WORK="${WORK:-/tmp/opencode/scribe-bench}"
RESULT_FILE="${1:-}"

if [[ ! -x "$SCRIBE_BIN" ]]; then
  echo "scribe binary not found at $SCRIBE_BIN (run: cargo build --release -p scribe-cli)" >&2
  exit 1
fi
if [[ ! -x "$SPHINX_BUILD" ]]; then
  echo "sphinx-build not found at $SPHINX_BUILD" >&2
  exit 1
fi
if [[ ! -x "$TIME" ]]; then
  echo "GNU time not found at $TIME" >&2
  exit 1
fi

rm -rf "$WORK"
mkdir -p "$WORK"
FIXTURE="$WORK/fixture"
"$PYTHON" "$ROOT/scripts/gen_bench_fixture.py" "$FIXTURE" "$PAGES" >/dev/null

# Convert "h:mm:ss" / "m:ss" from GNU time into fractional seconds.
to_seconds() {
  awk -F: '{
    if (NF == 3) print $1*3600 + $2*60 + $3;
    else if (NF == 2) print $1*60 + $2;
    else print $1;
  }' <<<"$1"
}

# Run one cold build; echo "elapsed_seconds max_rss_kb".
measure() {
  local label="$1"; shift
  local outdir="$WORK/out-$label"
  local timefile="$WORK/time-$label.txt"
  rm -rf "$outdir"
  mkdir -p "$outdir"
  "$TIME" -v -o "$timefile" "$@" >"$WORK/stdout-$label.txt" 2>"$WORK/stderr-$label.txt" || true
  local elapsed rss
  elapsed="$(grep 'Elapsed (wall clock)' "$timefile" | sed 's/.*: //' | xargs)"
  rss="$(grep 'Maximum resident set size' "$timefile" | awk '{print $NF}')"
  echo "$(to_seconds "$elapsed") $rss"
}

median() {
  printf '%s\n' "$@" | sort -n | awk '{a[NR]=$1} END {if (NR%2) print a[(NR+1)/2]; else print (a[NR/2]+a[NR/2+1])/2}'
}

run_engine() {
  local label="$1"; shift
  # Warmup (not measured).
  measure "$label-warmup" "$@" >/dev/null
  local i secs rss
  : >"$WORK/times-$label.txt"
  for ((i = 1; i <= RUNS; i++)); do
    read -r secs rss < <(measure "$label-$i" "$@")
    printf '%s %s\n' "$secs" "$rss" >>"$WORK/times-$label.txt"
    printf '  %s run %d: %ss, %s KB peak RSS\n' "$label" "$i" "$secs" "$rss" >&2
  done
}

run_engine scribe "$SCRIBE_BIN" build -b html -E -q "$FIXTURE" "$WORK/out-scribe"
run_engine sphinx "$SPHINX_BUILD" -b html -E -q "$FIXTURE" "$WORK/out-sphinx"

s_sec="$(awk '{print $1}' "$WORK/times-scribe.txt" | xargs printf '%s\n' | { mapfile -t a; median "${a[@]}"; })"
s_rss="$(awk '{print $2}' "$WORK/times-scribe.txt" | xargs printf '%s\n' | { mapfile -t a; median "${a[@]}"; })"
p_sec="$(awk '{print $1}' "$WORK/times-sphinx.txt" | xargs printf '%s\n' | { mapfile -t a; median "${a[@]}"; })"
p_rss="$(awk '{print $2}' "$WORK/times-sphinx.txt" | xargs printf '%s\n' | { mapfile -t a; median "${a[@]}"; })"

ratio="$(awk -v s="$s_sec" -v p="$p_sec" 'BEGIN { if (p > 0) printf "%.2f", p/s; else print "n/a" }')"
rss_ratio="$(awk -v s="$s_rss" -v p="$p_rss" 'BEGIN { if (s > 0) printf "%.2f", p/s; else print "n/a" }')"

pages="$(find "$FIXTURE" -name '*.rst' | wc -l | tr -d ' ')"
s_html="$(find "$WORK/out-scribe" -name '*.html' | wc -l | tr -d ' ')"
p_html="$(find "$WORK/out-sphinx" -name '*.html' | wc -l | tr -d ' ')"
scribe_ver="$("$SCRIBE_BIN" --version 2>/dev/null | head -1)"
sphinx_ver="$("$SPHINX_BUILD" --version 2>/dev/null | head -1)"

report="$(cat <<EOF
# Benchmark results (measured $(date -u +%Y-%m-%dT%H:%M:%SZ) UTC)

- Fixture: synthetic offline multi-page project, ${pages} source pages
  (\`scripts/gen_bench_fixture.py\`; sections, inline markup, lists,
  admonitions, code blocks, simple tables, toctree/xrefs; no extensions).
- Command: \`-b html -E -q\` (cold build, fresh environment each run).
- Runs: 1 warmup + ${RUNS} measured per engine; values below are medians.
- Toolchain: \`$scribe_ver\`, \`$sphinx_ver\`.
- Host: $(uname -srm), $(nproc) logical CPUs, $(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | sed 's/.*: //' || echo unknown).

| Engine | Median wall (s) | Peak RSS (KB) | HTML pages |
|---|---:|---:|---:|
| scribe | $s_sec | $s_rss | $s_html |
| sphinx-build | $p_sec | $p_rss | $p_html |

Measured speedup (sphinx-build / scribe wall time): **${ratio}x**.
Peak-RSS ratio (sphinx-build / scribe): **${rss_ratio}x**.
EOF
)"

echo "$report"
if [[ -n "$RESULT_FILE" ]]; then
  echo "$report" >"$RESULT_FILE"
  echo "wrote $RESULT_FILE" >&2
fi
