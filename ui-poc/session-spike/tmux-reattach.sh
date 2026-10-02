#!/usr/bin/env bash
# tmux reattach check: does a pane survive its control-mode client dying,
# and how does a new client recover the screen? (control mode sends no
# screen state on attach; the client must ask with capture-pane.)
set -eu
T=(tmux -L quark-spike-reattach)
"${T[@]}" kill-server 2>/dev/null || true
"${T[@]}" -f /dev/null start-server \; set -g history-limit 10000 \; \
  new-session -d -s s -x 120 -y 36 \
  "bash -c 'i=0; while true; do i=\$((i+1)); echo line \$i; sleep 0.002; done'"
sleep 4
pid0=$("${T[@]}" display -p -t s '#{pane_pid}')
# A control client that streams for 1 s, then is killed (simulated quarkd crash).
{ sleep 1; } | "${T[@]}" -C attach -t s >/dev/null & ctl=$!
sleep 0.5; kill -9 "$ctl" 2>/dev/null || true; wait "$ctl" 2>/dev/null || true
sleep 2
pid1=$("${T[@]}" display -p -t s '#{pane_pid}')
echo "- pane pid before $pid0, after control client was killed $pid1 (same = survived)"
s=$(date +%s%N); vis=$("${T[@]}" capture-pane -p -e -t s | wc -l); e=$(date +%s%N)
echo "- capture-pane -e (visible screen, with SGR): $vis rows in $(( (e - s) / 1000000 )) ms"
s=$(date +%s%N); all=$("${T[@]}" capture-pane -p -e -S - -t s | wc -l); e=$(date +%s%N)
echo "- capture-pane -e -S - (all history): $all rows in $(( (e - s) / 1000000 )) ms"
"${T[@]}" kill-server
