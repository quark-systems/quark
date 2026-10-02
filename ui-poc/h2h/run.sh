#!/usr/bin/env bash
# Head-to-head bench: native (warpui + alacritty_terminal) vs Tauri (xterm.js WebGL/DOM, wterm, ghostty-web).
# Each run gets a fresh stub daemon so earlier chat replies and floods can't leak between runs.
# Env: ROUNDS (3), RENDERERS (Tauri ?renderer= values, default "webgl dom"; also wterm, wterm-lite,
# ghostty-web), NATIVE (1 = include the native build, 0 = skip), TAURI_TIMEOUT (60 s; per-phase
# QUARK_BENCH_PROGRESS lines are kept in tauri-*.log even when a run times out), PORT (7440), OUT, DISPLAY (:96).
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${OUT:-$ROOT/h2h/results}; mkdir -p "$OUT"
PORT=${PORT:-7440}; DISPLAY=${DISPLAY:-:96}; export DISPLAY
export XDG_RUNTIME_DIR=/tmp/xdg-h2h; mkdir -p -m 700 $XDG_RUNTIME_DIR
pgrep -f "Xvfb $DISPLAY" >/dev/null || { Xvfb $DISPLAY -screen 0 1600x1000x24 >/dev/null 2>&1 & sleep 1; }
ROUNDS=${ROUNDS:-3}
RENDERERS=${RENDERERS:-webgl dom}
NATIVE=${NATIVE:-1}

stub_up()   { (cd "$ROOT/stub-daemon" && ./target/release/quark-ui-stub-daemon --port $PORT >/dev/null 2>&1 &); sleep 4; }
stub_down() { pkill -f "quark-ui-stub-daemon --port $PORT"; sleep 1; tmux -L quark-poc-$PORT kill-server 2>/dev/null; sleep 1; }

for r in $(seq 1 "$ROUNDS"); do
  if [ "$NATIVE" = 1 ]; then
    stub_up
    timeout 60 "$ROOT/native/target/release/quark-ui-native-poc" --daemon http://127.0.0.1:$PORT --bench 20 \
      2>/dev/null > "$OUT/native-$r.json"
    stub_down
  fi
  for rend in $RENDERERS; do
    stub_up
    q="bench=1&exit=1&daemon=http://127.0.0.1:$PORT"; [ "$rend" != webgl ] && q="$q&renderer=$rend"
    QUARK_QUERY="$q" timeout ${TAURI_TIMEOUT:-60} "$ROOT/tauri/src-tauri/target/release/quark-ui-poc-tauri" 2>/dev/null \
      | tee "$OUT/tauri-$rend-$r.log" | grep QUARK_BENCH_RESULT | tail -1 | sed 's/^.*QUARK_BENCH_RESULT //' > "$OUT/tauri-$rend-$r.json"
    stub_down
  done
  echo "round $r done"
done
