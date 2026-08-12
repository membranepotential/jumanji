#!/usr/bin/env bash
# Headless startup-time benchmark: wall-clock time from process spawn until
# the reader is driveable (its D-Bus GetState reports `"loaded":true`).
#
# Same isolation approach as tests/e2e.rs: a private Xvfb display and a
# private session bus per run, torn down afterwards. Nothing here touches the
# developer's live DISPLAY or session bus.
#
# Usage: scripts/bench-startup.sh [-n RUNS]
#   -n RUNS   number of runs per fixture (default 5). Reports per-run ms and
#             the median for each fixture.
#
# Fixtures (both run by default):
#   - demo/demo.md
#   - a generated wikilink-heavy note in a temp vault (50 sibling notes + a
#     main note with 100 [[wikilinks]]), which exercises the vault-index path
#     (the initial render defers until the background scan lands).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

RUNS=5
while getopts "n:h" opt; do
    case "$opt" in
    n) RUNS="$OPTARG" ;;
    h)
        sed -n '2,20p' "$0"
        exit 0
        ;;
    *)
        echo "usage: $0 [-n RUNS]" >&2
        exit 1
        ;;
    esac
done

INTERFACE="org.membranepotential.jumanji"
OBJECT_PATH="/org/membranepotential/jumanji"
POLL_INTERVAL="0.005"
LOAD_TIMEOUT_US=$((20 * 1000 * 1000))

# ---------------------------------------------------------------------------
# Environment gate
# ---------------------------------------------------------------------------

missing=()
for tool in Xvfb dbus-daemon gdbus; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
if [ "${#missing[@]}" -gt 0 ]; then
    echo "bench-startup: skipping — missing ${missing[*]} on PATH" \
        "(Arch: pacman -S xorg-server-xvfb dbus glib2). Nothing to report."
    exit 0
fi

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

BIN="$REPO_ROOT/target/release/jumanji"
if [ ! -x "$BIN" ] || [ -n "$(find src -type f -newer "$BIN" 2>/dev/null)" ]; then
    echo "bench-startup: building release binary..."
    cargo build --release
fi

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

WORK_DIR="$(mktemp -d /tmp/jumanji-bench-startup.XXXXXX)"
trap 'rm -rf "$WORK_DIR"' EXIT

# 50 sibling notes + a main note with 100 wikilinks (round-robin over the 50),
# to exercise the vault-index path (deferred initial render + background scan).
gen_wikilink_vault() {
    local dir="$WORK_DIR/vault"
    mkdir -p "$dir"
    local i
    for i in $(seq 1 50); do
        printf '# Note %d\n\nSome content for note %d.\n' "$i" "$i" >"$dir/Note $i.md"
    done
    {
        printf '# Main\n\n'
        for i in $(seq 1 100); do
            printf 'See [[Note %d]] for details.\n\n' "$(((i % 50) + 1))"
        done
    } >"$dir/Main.md"
    echo "$dir/Main.md"
}

WIKILINK_MAIN="$(gen_wikilink_vault)"

# ---------------------------------------------------------------------------
# Timing helpers
# ---------------------------------------------------------------------------

# Microseconds since the epoch, pure bash (no fork) — cheap enough to call in
# a tight poll loop.
epoch_us() {
    local t="$EPOCHREALTIME"
    local sec="${t%.*}" usec="${t#*.}"
    echo $((sec * 1000000 + 10#$usec))
}

next_display=$((150 + ($$ % 200)))

# ---------------------------------------------------------------------------
# One run: launch Xvfb + a private bus + the reader on $1, time until loaded.
# Prints the elapsed milliseconds (integer, rounded) on stdout.
# ---------------------------------------------------------------------------
run_once() {
    local file="$1"
    local display=$next_display
    next_display=$((next_display + 1))
    local display_arg=":$display"

    local xvfb_pid dbus_pid app_pid
    local addr_file="$WORK_DIR/dbus-addr-$$-$display"
    local config_home="$WORK_DIR/xdg-config-$display"
    local data_home="$WORK_DIR/xdg-data-$display"
    mkdir -p "$config_home" "$data_home"

    Xvfb "$display_arg" -screen 0 1280x1024x24 >/dev/null 2>&1 &
    xvfb_pid=$!
    local deadline=$(($(epoch_us) + 10 * 1000 * 1000))
    while [ ! -e "/tmp/.X11-unix/X$display" ]; do
        if [ "$(epoch_us)" -ge "$deadline" ]; then
            echo "bench-startup: Xvfb socket for :$display never appeared" >&2
            kill "$xvfb_pid" 2>/dev/null || true
            return 1
        fi
        sleep 0.05
    done

    dbus-daemon --session --print-address=1 --nofork --nopidfile >"$addr_file" 2>/dev/null &
    dbus_pid=$!
    deadline=$(($(epoch_us) + 10 * 1000 * 1000))
    while [ ! -s "$addr_file" ]; do
        if [ "$(epoch_us)" -ge "$deadline" ]; then
            echo "bench-startup: dbus-daemon produced no address" >&2
            kill "$xvfb_pid" "$dbus_pid" 2>/dev/null || true
            return 1
        fi
        sleep 0.05
    done
    local dbus_addr
    dbus_addr="$(head -n1 "$addr_file")"

    local start
    start=$(epoch_us)
    DISPLAY="$display_arg" \
        DBUS_SESSION_BUS_ADDRESS="$dbus_addr" \
        XDG_CONFIG_HOME="$config_home" \
        XDG_DATA_HOME="$data_home" \
        "$BIN" "$file" --foreground </dev/null >/dev/null 2>&1 &
    app_pid=$!

    local dest="$INTERFACE.PID-$app_pid"
    local deadline_us=$((start + LOAD_TIMEOUT_US))
    local loaded=0
    local state
    while [ "$(epoch_us)" -lt "$deadline_us" ]; do
        if state=$(DBUS_SESSION_BUS_ADDRESS="$dbus_addr" gdbus call --session \
            --dest "$dest" \
            --object-path "$OBJECT_PATH" \
            --method "$INTERFACE.GetState" 2>/dev/null); then
            case "$state" in
            *'"loaded":true'*)
                loaded=1
                break
                ;;
            esac
        fi
        sleep "$POLL_INTERVAL"
    done
    local end
    end=$(epoch_us)

    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
    kill "$dbus_pid" 2>/dev/null || true
    wait "$dbus_pid" 2>/dev/null || true
    kill "$xvfb_pid" 2>/dev/null || true
    wait "$xvfb_pid" 2>/dev/null || true

    if [ "$loaded" -ne 1 ]; then
        echo "bench-startup: timed out waiting for $file to load" >&2
        return 1
    fi

    echo $(((end - start) / 1000))
}

# ---------------------------------------------------------------------------
# Drive N runs per fixture, report per-run ms and the median.
# ---------------------------------------------------------------------------
bench_fixture() {
    local label="$1" file="$2"
    local times=()
    local i ms
    for i in $(seq 1 "$RUNS"); do
        ms="$(run_once "$file")"
        times+=("$ms")
        echo "  run $i: ${ms} ms"
    done
    local median
    median="$(printf '%s\n' "${times[@]}" | sort -n | awk '{a[NR]=$1} END {print (NR%2==1) ? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2}')"
    echo "$label median: ${median} ms (n=$RUNS)"
    echo
}

echo "bench-startup: $RUNS run(s) per fixture"
echo
bench_fixture "demo/demo.md" "$REPO_ROOT/demo/demo.md"
bench_fixture "wikilink-heavy (50 notes, 100 links)" "$WIKILINK_MAIN"
