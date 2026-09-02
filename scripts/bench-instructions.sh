#!/usr/bin/env bash
# Pipeline cost in instructions retired, per render, per fixture — the
# deterministic complement to the wall-clock criterion benches.
#
# Wall clock drifts with CPU frequency and runner type (±30% between shared
# runners; up to 2× on a throttling laptop, measured on identical binaries).
# The instructions a render retires do not: valgrind's cachegrind counts them
# exactly, so a 3% change is a change in the code, not in the weather.
#
# Per fixture, the bench binary's `--once` mode is run twice under cachegrind,
# with `--repeat 0` and `--repeat N`; the runs differ by exactly N renders, so
# `(I_N − I_0) / N` is one render's count with start-up and the one-time
# syntect/math initialisation cancelled out. Rayon is pinned to one thread so
# the parallel highlighter's work-stealing (spin loops, variable partitioning)
# cannot leak into the count; the work is the same.
#
# Usage: scripts/bench-instructions.sh [-b BENCH_BIN] [-j FILE] [-n N]
#   -b BIN    a prebuilt pipeline bench executable (default: build it)
#   -j FILE   also write the results as JSON (`customSmallerIsBetter` shape,
#             for the CI trail)
#   -n N      renders in the counted run (default 5)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

BIN=""
JSON_OUT=""
REPEAT=5
while getopts "b:j:n:h" opt; do
    case "$opt" in
    b) BIN="$OPTARG" ;;
    j) JSON_OUT="$OPTARG" ;;
    n) REPEAT="$OPTARG" ;;
    h)
        sed -n '2,22p' "$0"
        exit 0
        ;;
    *)
        echo "usage: $0 [-b BENCH_BIN] [-j FILE] [-n N]" >&2
        exit 1
        ;;
    esac
done

command -v valgrind >/dev/null 2>&1 || {
    echo "bench-instructions: valgrind not found (Arch: pacman -S valgrind)" >&2
    exit 1
}

# The bench executable a `cargo bench --no-run` produced, from cargo's JSON.
bench_binary() {
    cargo bench --quiet --bench pipeline --no-run --message-format=json 2>/dev/null |
        python3 -c 'import json,sys
for line in sys.stdin:
    try: m = json.loads(line)
    except ValueError: continue
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["target"]["name"] == "pipeline":
        print(m["executable"])'
}

if [ -z "$BIN" ]; then
    BIN="$(bench_binary)"
fi
[ -x "$BIN" ] || {
    echo "bench-instructions: no bench executable at '$BIN'" >&2
    exit 1
}

# Instructions retired by one `--once` run: cachegrind's `I refs` total.
count() {
    local fixture="$1" repeat="$2"
    RAYON_NUM_THREADS=1 valgrind --tool=cachegrind --cache-sim=no \
        --cachegrind-out-file=/dev/null "$BIN" --once "$fixture" --repeat "$repeat" 2>&1 |
        sed -n 's/^==[0-9]*== *I *refs: *\([0-9,]*\).*/\1/p' | tr -d ','
}

FIXTURES="$("$BIN" --bench --list 2>/dev/null | sed -n 's/^pipeline::render\/\(.*\): benchmark$/\1/p')"
[ -n "$FIXTURES" ] || {
    echo "bench-instructions: could not list the fixtures" >&2
    exit 1
}

JSON_ENTRIES=()
printf '%-14s %18s\n' "fixture" "instructions/render"
for fixture in $FIXTURES; do
    i0="$(count "$fixture" 0)"
    in="$(count "$fixture" "$REPEAT")"
    [ -n "$i0" ] && [ -n "$in" ] || {
        echo "bench-instructions: no instruction count for '$fixture' — does the bench binary support --once?" >&2
        exit 1
    }
    per=$(((in - i0) / REPEAT))
    printf '%-14s %18s\n' "$fixture" "$per"
    JSON_ENTRIES+=("{\"name\": \"instructions: $fixture\", \"unit\": \"instructions/render\", \"value\": $per}")
done

if [ -n "$JSON_OUT" ]; then
    {
        echo "["
        printf '  %s' "${JSON_ENTRIES[0]}"
        for e in "${JSON_ENTRIES[@]:1}"; do printf ',\n  %s' "$e"; done
        printf '\n]\n'
    } >"$JSON_OUT"
    echo "bench-instructions: wrote $JSON_OUT"
fi
