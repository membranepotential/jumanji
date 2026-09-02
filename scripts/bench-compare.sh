#!/usr/bin/env bash
# Local performance A/B: a git ref against the working tree.
#
# Builds the ref in a throwaway worktree with its own target dir, then runs
# both binaries through the same measurements and prints one table:
#
#   - startup time (scripts/bench-startup.sh), the two builds INTERLEAVED run
#     by run so a machine that warms or cools mid-bench skews both sides
#     equally;
#   - the criterion pipeline benches, the ref saved as a criterion baseline
#     and the tree compared against it (criterion prints the change and its
#     p-value per bench).
#
# Run it on a quiet machine — no builds, no test suites, nothing in the
# background — or the numbers mean nothing. The suite's own rule.
#
# Usage: scripts/bench-compare.sh [-n RUNS] [REF]
#   REF       the baseline ref (default: the latest tag)
#   -n RUNS   startup runs per side (default 10)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

RUNS=10
while getopts "n:h" opt; do
    case "$opt" in
    n) RUNS="$OPTARG" ;;
    h)
        sed -n '2,20p' "$0"
        exit 0
        ;;
    *)
        echo "usage: $0 [-n RUNS] [REF]" >&2
        exit 1
        ;;
    esac
done
shift $((OPTIND - 1))
REF="${1:-$(git describe --tags --abbrev=0)}"
REF_SHA="$(git rev-parse --short "$REF")"

WORKTREE="$REPO_ROOT/.bench-baseline/$REF_SHA"
BASE_TARGET="$REPO_ROOT/target-baseline"

echo "bench-compare: baseline $REF ($REF_SHA) vs. working tree"
governor="$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo unknown)"
if [ "$governor" != "performance" ]; then
    echo "bench-compare: note — CPU governor is '$governor'; frequency drifts ±30%/min here." \
        "Both sides are interleaved so the deltas hold, but absolute figures will not."
fi
echo

# ---------------------------------------------------------------------------
# Build both sides, release profile
# ---------------------------------------------------------------------------

if [ ! -d "$WORKTREE" ]; then
    mkdir -p "$(dirname "$WORKTREE")"
    git worktree add --detach "$WORKTREE" "$REF" >/dev/null
fi
echo "bench-compare: building baseline..."
(cd "$WORKTREE" && CARGO_TARGET_DIR="$BASE_TARGET" cargo build --release --quiet)
BASE_BIN="$BASE_TARGET/release/jumanji"

echo "bench-compare: building working tree..."
cargo build --release --quiet
TREE_BIN="$REPO_ROOT/target/release/jumanji"
echo

# ---------------------------------------------------------------------------
# Startup: interleaved single runs, medians per side
# ---------------------------------------------------------------------------

median() {
    sort -n | awk '{a[NR]=$1} END {print (NR%2==1) ? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2}'
}

# One `bench-startup.sh -n 1` run prints "<label> median: <ms> ms" per
# fixture; collect the ms per fixture per side.
declare -A BASE_T TREE_T
collect() {
    local side="$1" bin="$2"
    local line label ms
    while IFS= read -r line; do
        case "$line" in
        *" median: "*" ms (n=1)")
            label="${line%% median: *}"
            ms="${line#* median: }"
            ms="${ms%% ms*}"
            if [ "$side" = base ]; then
                BASE_T["$label"]+="$ms"$'\n'
            else
                TREE_T["$label"]+="$ms"$'\n'
            fi
            ;;
        esac
    done < <(scripts/bench-startup.sh -n 1 -b "$bin" 2>/dev/null)
}

echo "bench-compare: startup, $RUNS interleaved runs per side..."
for i in $(seq 1 "$RUNS"); do
    collect base "$BASE_BIN"
    collect tree "$TREE_BIN"
    printf '  %d/%d\r' "$i" "$RUNS"
done
echo

printf '%-40s %10s %10s %8s\n' "startup (median ms)" "$REF_SHA" "tree" "delta"
for label in "${!BASE_T[@]}"; do
    b="$(printf '%s' "${BASE_T[$label]}" | median)"
    t="$(printf '%s' "${TREE_T[$label]}" | median)"
    d="$(awk -v b="$b" -v t="$t" 'BEGIN { printf "%+.1f%%", (t - b) / b * 100 }')"
    printf '%-40s %10s %10s %8s\n' "$label" "$b" "$t" "$d"
done
echo

# ---------------------------------------------------------------------------
# Pipeline: instructions retired per render, both sides, via
# scripts/bench-instructions.sh (cachegrind). Deterministic, so identical
# code reads 0.0% and a delta is a delta — unlike wall clock, which drifted
# by up to 2× between byte-identical binaries on this laptop in every
# ordering that was tried.
# ---------------------------------------------------------------------------

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

# The ref is measured with the TREE's bench harness: the instruction-count
# mode (`--once`) may postdate the ref, and the harness only touches
# `core::pipeline` / `core::vault`, which any ref worth comparing has. The
# worktree is throwaway, so the overlay is never committed anywhere.
echo "bench-compare: building the pipeline benches for both sides..."
cp "$REPO_ROOT/benches/pipeline.rs" "$WORKTREE/benches/pipeline.rs"
BASE_BENCH="$(cd "$WORKTREE" && CARGO_TARGET_DIR="$REPO_ROOT/target" bench_binary)"
TREE_BENCH="$(bench_binary)"
[ -x "$BASE_BENCH" ] && [ -x "$TREE_BENCH" ] || {
    echo "bench-compare: could not locate both bench binaries" >&2
    exit 1
}

echo "bench-compare: pipeline instruction counts (cachegrind), both sides..."
declare -A BASE_I TREE_I
while read -r fixture per; do BASE_I["$fixture"]="$per"; done \
    < <(scripts/bench-instructions.sh -b "$BASE_BENCH" | awk 'NR > 1 { print $1, $2 }')
while read -r fixture per; do TREE_I["$fixture"]="$per"; done \
    < <(scripts/bench-instructions.sh -b "$TREE_BENCH" | awk 'NR > 1 { print $1, $2 }')

printf '%-24s %16s %16s %8s\n' "pipeline (instr/render)" "$REF_SHA" "tree" "delta"
for fixture in "${!TREE_I[@]}"; do
    b="${BASE_I[$fixture]:-0}"
    t="${TREE_I[$fixture]}"
    d="$(awk -v b="$b" -v t="$t" 'BEGIN { if (b > 0) printf "%+.2f%%", (t - b) / b * 100; else print "n/a" }')"
    printf '%-24s %16s %16s %8s\n' "$fixture" "$b" "$t" "$d"
done | sort
echo
echo "bench-compare: done. Baseline worktree kept at $WORKTREE (git worktree remove it when finished)."
