#!/usr/bin/env bash
# Benchmark scribe vs. sphinx-build on real-world Python documentation trees.
#
# Shallow-clones five repositories into a temp workspace, runs cold `-E -q`
# HTML builds with both engines, and reports exit status, warning counts, HTML
# page counts, median wall-clock time and peak RSS. Every number is measured
# on the current machine; nothing is hard-coded.
#
# Usage:
#   ./scripts/corpus_bench.sh [results.md]
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIBE_BIN="${SCRIBE_BIN:-$ROOT/target/release/scribe}"
SPHINX_BUILD="${SPHINX_BUILD:-$ROOT/.oracle-venv/bin/sphinx-build}"
TIME="${TIME:-/usr/bin/time}"
WORK="${WORK:-/tmp/scribe-corpus}"
RUNS="${RUNS:-3}"
RESULT_FILE="${1:-}"

# name|clone URL|docs directory (relative to clone root)
REPOS=(
  "flask|https://github.com/pallets/flask|docs"
  "requests|https://github.com/psf/requests|docs"
  "pytest|https://github.com/pytest-dev/pytest|doc/en"
  "sqlalchemy|https://github.com/sqlalchemy/sqlalchemy|doc/build"
  "django|https://github.com/django/django|docs"
)

for tool in "$SCRIBE_BIN" "$SPHINX_BUILD" "$TIME"; do
  if [[ ! -x "$tool" ]]; then echo "missing executable: $tool" >&2; exit 1; fi
done

mkdir -p "$WORK"

clone_repo() {
  local name="$1" url="$2" sparse="$3" dest="$WORK/$name"
  if [[ -f "$dest/$sparse/conf.py" ]]; then return 0; fi
  echo "cloning $url (docs only) ..." >&2
  rm -rf "$dest"
  git clone --depth 1 --filter=blob:none --sparse "$url" "$dest" >/dev/null 2>&1 \
    && git -C "$dest" sparse-checkout set "$sparse" >/dev/null 2>&1
}

# Convert GNU time's h:mm:ss / m:ss to fractional seconds.
to_seconds() {
  awk -F: '{
    if (NF == 3) print $1*3600 + $2*60 + $3;
    else if (NF == 2) print $1*60 + $2;
    else print $1;
  }' <<<"$1"
}

# Runs one measured build. Echoes: exit_code elapsed_seconds max_rss_kb
time_build() {
  local label="$1" out="$2"; shift 2
  local timefile="$WORK/time-$label.txt"
  rm -rf "$out"; mkdir -p "$out"
  "$TIME" -v -o "$timefile" "$@" >"$WORK/stdout-$label.txt" 2>"$WORK/stderr-$label.txt"
  local code=$?
  local elapsed rss
  elapsed="$(grep 'Elapsed (wall clock)' "$timefile" 2>/dev/null | sed 's/.*: //' | xargs)"
  rss="$(grep 'Maximum resident set size' "$timefile" 2>/dev/null | awk '{print $NF}')"
  echo "$code $(to_seconds "${elapsed:-0}") ${rss:-0}"
}

count_warnings() {
  cat "$WORK/stdout-$1.txt" "$WORK/stderr-$1.txt" 2>/dev/null | grep -c 'WARNING:' || true
}

count_html() {
  find "$1" -name '*.html' 2>/dev/null | wc -l | tr -d ' '
}

median() {
  printf '%s\n' "$@" | sort -n | awk '{a[NR]=$1} END {if (NR%2) print a[(NR+1)/2]; else print (a[NR/2]+a[NR/2+1])/2}'
}

# Probe a build once, retrying with `-D html_theme=basic` when only the theme
# is unbundled. Echoes: theme_override(0|1)
probe() {
  local engine="$1" srcdir="$2" out="$3"; shift 3
  local -a cmd=("$@")
  local result override=0
  result="$(time_build "probe-$engine" "$out" "${cmd[@]}")"
  local stderr="$WORK/stderr-probe-$engine.txt"
  if grep -qiE "no theme named|ThemeError|unknown theme|theme '[^']*' (inherits from|is not a loaded)" "$stderr" 2>/dev/null; then
    # Retry with the bundled theme. `-D` must precede the positional src/out
    # arguments, so insert it just before the final output directory.
    local n=${#cmd[@]}
    local -a retry=("${cmd[@]:0:n-1}" -D html_theme=basic "${cmd[@]:n-1}")
    result="$(time_build "probe-$engine" "$out" "${retry[@]}")"
    override=1
  fi
  printf '%s\n' "$result" >"$WORK/probe-$engine.txt"
  echo "$override"
}

# Measure one engine; echoes "code seconds rss warnings pages" (median of RUNS).
measure_engine() {
  local engine="$1" srcdir="$2" theme_override="$3"; shift 3
  local -a cmd=("$@")
  if [[ "$theme_override" == "1" ]]; then
    local n=${#cmd[@]}
    cmd=("${cmd[@]:0:n-1}" -D html_theme=basic "${cmd[@]:n-1}")
  fi
  local out="$WORK/measure-out-$engine"
  # Unmeasured warmup.
  time_build "warm-$engine" "$out" "${cmd[@]}" >/dev/null
  local i code secs rss
  : >"$WORK/times-$engine.txt"
  for ((i = 1; i <= RUNS; i++)); do
    read -r code secs rss < <(time_build "run-$engine-$i" "$out" "${cmd[@]}")
    printf '%s %s %s\n' "$secs" "$rss" "$code" >>"$WORK/times-$engine.txt"
    printf '  %s run %d: exit=%s %ss, %s KB\n' "$engine" "$i" "$code" "$secs" "$rss" >&2
  done
  local sec_med rss_med
  sec_med="$(awk '{print $1}' "$WORK/times-$engine.txt" | { mapfile -t a; median "${a[@]}"; })"
  rss_med="$(awk '{print $2}' "$WORK/times-$engine.txt" | { mapfile -t a; median "${a[@]}"; })"
  echo "$code $(count_warnings "run-$engine-$RUNS") $(count_html "$WORK/measure-out-$engine") $sec_med $rss_med"
}

scribe_ver="$("$SCRIBE_BIN" --version 2>/dev/null | head -1)"
sphinx_ver="$("$SPHINX_BUILD" --version 2>/dev/null | head -1)"

results=()
for spec in "${REPOS[@]}"; do
  IFS='|' read -r name url sparse <<<"$spec"
  srcdir="$WORK/$name/$sparse"
  clone_repo "$name" "$url" "$sparse"
  echo "== $name ($srcdir) ==" >&2

  scribe_out="$WORK/probe-out-scribe"
  sphinx_out="$WORK/probe-out-sphinx"
  scribe_override="$(probe scribe "$srcdir" "$scribe_out" \
    "$SCRIBE_BIN" build -b html -E -q -c "$srcdir" "$srcdir" "$scribe_out")"
  sphinx_override="$(probe sphinx "$srcdir" "$sphinx_out" \
    "$SPHINX_BUILD" -b html -E -q -c "$srcdir" "$srcdir" "$sphinx_out")"

  scribe_probe_code=$(awk '{print $1}' "$WORK/probe-scribe.txt" 2>/dev/null)
  sphinx_probe_code=$(awk '{print $1}' "$WORK/probe-sphinx.txt" 2>/dev/null)

  scribe_line="n/a"
  if [[ "$scribe_probe_code" == "0" ]]; then
    scribe_line="$(measure_engine scribe "$srcdir" "$scribe_override" \
      "$SCRIBE_BIN" build -b html -E -q -c "$srcdir" "$srcdir" "$WORK/measure-out-scribe")"
  fi
  sphinx_line="n/a"
  if [[ "$sphinx_probe_code" == "0" ]]; then
    sphinx_line="$(measure_engine sphinx "$srcdir" "$sphinx_override" \
      "$SPHINX_BUILD" -b html -E -q -c "$srcdir" "$srcdir" "$WORK/measure-out-sphinx")"
  fi

  results+=("$name|$scribe_line|$sphinx_line|$scribe_override|$sphinx_override")
done

report="$(cat <<EOF
# Real-world corpus benchmark (measured $(date -u +%Y-%m-%dT%H:%M:%SZ) UTC)

- Toolchain: \`$scribe_ver\`, \`$sphinx_ver\`.
- Host: $(uname -srm), $(nproc) logical CPUs, $(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | sed 's/.*: //' || echo unknown).
- Command: \`build -b html -E -q <srcdir> <out>\` (cold build; output removed every run).
- Runs: 1 warmup + $RUNS measured per engine; values below are medians.
- Timer: GNU \`/usr/bin/time -v\` (wall clock + maximum resident set size).

| Repository | Engine | Exit | Warnings | HTML pages | Median wall (s) | Peak RSS (KB) |
|---|---|---:|---:|---:|---:|---:|
EOF
)"
for row in "${results[@]}"; do
  IFS='|' read -r name s_line p_line s_override p_override <<<"$row"
  read -r s_code s_warn s_html s_sec s_rss <<<"$s_line"
  read -r p_code p_warn p_html p_sec p_rss <<<"$p_line"
  report+=$'\n'"| $name | scribe | ${s_code:-n/a} | ${s_warn:-n/a} | ${s_html:-n/a} | ${s_sec:-n/a} | ${s_rss:-n/a} |"
  report+=$'\n'"| $name | sphinx-build | ${p_code:-n/a} | ${p_warn:-n/a} | ${p_html:-n/a} | ${p_sec:-n/a} | ${p_rss:-n/a} |"
done

if [[ -n "$RESULT_FILE" ]]; then
  printf '%s\n' "$report" >"$RESULT_FILE"
  echo "wrote $RESULT_FILE" >&2
else
  printf '%s\n' "$report"
fi
