#!/usr/bin/env bash
# One headless-Chromium bench per renderer, fresh stub per run (same pattern as ../h2h/run.sh).
# Needs the built frontend served at APP (default: `npx vite preview --port 4173`).
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT=${OUT:-$ROOT/h2h/results-renderers}; mkdir -p "$OUT"
PORT=${PORT:-7461}; APP=${APP:-http://127.0.0.1:4173/}
RENDERERS=${RENDERERS:-webgl dom wterm wterm-lite ghostty-web}
stub_up()   { (cd "$ROOT/stub-daemon" && ./target/release/quark-ui-stub-daemon --port $PORT >/dev/null 2>&1 &); sleep 4; }
stub_down() { pkill -f "quark-ui-stub-daemon --port $PORT"; sleep 1; tmux -L quark-poc-$PORT kill-server 2>/dev/null; sleep 1; }
for rend in $RENDERERS; do
  stub_up
  node "$ROOT/tauri/scripts/web-bench.mjs" "${APP}?bench=1&renderer=$rend&daemon=http://127.0.0.1:$PORT" \
    | grep QUARK_BENCH_RESULT | sed 's/^QUARK_BENCH_RESULT //' > "$OUT/chromium-$rend-1.json"
  stub_down
  echo "chromium $rend done"
done
