#!/usr/bin/env bash
# Regenerate the document-graph screenshots in this folder from demo/graph/.
#
# Headless: runs the release build under its own Xvfb and D-Bus session, with
# throwaway XDG dirs, so it never touches your desktop, config or history.
# Needs: xorg-server-xvfb, xdotool, dbus, imagemagick; `cargo build --release`.
#
#   docs/graph/screenshots/capture.sh
#
# WebKit renders at 2x under Xvfb here, so the 3000x1400 window is a
# 1500x700 CSS-px page; images are scaled back to 1500 px wide.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
bin="$repo/target/release/jumanji"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

if [[ -z "${JMNJ_CAPTURE_INNER:-}" ]]; then
  Xvfb :97 -screen 0 3000x1400x24 >/dev/null 2>&1 &
  xvfb=$!
  trap 'kill $xvfb 2>/dev/null; rm -rf "$tmp"' EXIT
  sleep 1
  JMNJ_CAPTURE_INNER=1 DISPLAY=:97 dbus-run-session -- "$0"
  exit
fi

export XDG_DATA_HOME="$tmp/data" XDG_CONFIG_HOME="$tmp/config" XDG_CACHE_HOME="$tmp/cache"
mkdir -p "$XDG_DATA_HOME" "$XDG_CONFIG_HOME"

cd "$repo/demo/graph"
"$bin" README.md >"$tmp/run.log" 2>&1 &
pid=$!
dest="org.membranepotential.jumanji.PID-$pid"
call() {
  gdbus call --session --dest "$dest" --object-path /org/membranepotential/jumanji \
    --method "org.membranepotential.jumanji.$1" "${@:2}"
}
# Run graph actions over D-Bus; `sleepN` pauses N seconds (walks land async).
act() {
  for a in "$@"; do
    case $a in
      sleep*) sleep "${a#sleep}" ;;
      *) call ExecuteAction "$a" 1 >/dev/null ;;
    esac
  done
}
shot() { import -window root "$tmp/$1.png"; magick "$tmp/$1.png" -resize 1500x -strip "$here/$1.png"; }

sleep 3
win=$(xdotool search --pid "$pid" | tail -1)
xdotool windowsize "$win" 3000 1400 windowmove "$win" 0 0
sleep 1

# The route README -> Architecture -> Storage, walked through the graph itself.
act recolor "toggle graph" sleep1.5 "graph child" "graph next" "graph next" "graph next" "graph open" sleep3 \
  "toggle graph" sleep2 "graph child" "graph next" "graph next" "graph open" sleep3 "toggle graph" sleep2
xdotool mousemove 2990 1300
sleep 0.8
shot links

# Peek: select the folded node above Storage and put the real pointer on it.
act "graph previous" sleep0.8
state=$(call GetState)
x=$(grep -o '"graph_sel_x":[0-9.]*' <<<"$state" | cut -d: -f2)
y=$(grep -o '"graph_sel_y":[0-9.]*' <<<"$state" | cut -d: -f2)
xdotool mousemove "$(python3 -c "print(int(float('$x') * 2 + 60))")" "$(python3 -c "print(int(float('$y') * 2 + 20))")"
sleep 1.2
shot peek
xdotool mousemove 2990 1300
sleep 0.5

act "graph view" sleep0.8 "graph next" sleep0.8
shot tree

act "graph view" sleep0.5
for _ in $(seq 12); do act "zoom out"; done
sleep 1
shot far

kill "$pid"
