#!/usr/bin/env bash
# Pane wrapper: loop.sh <script> <stress-flag-file>
# Waits until the daemon's control client is attached, then runs <script>
# (or flood.sh while the flag file exists), restarting it whenever it exits.
# The INT trap is a handler, not an ignore, so Ctrl-C does not kill this
# wrapper while child programs still get default Ctrl-C behaviour.
trap ':' INT
dir=$(dirname "$0")
while [ ! -e "$dir/attached" ]; do sleep 0.05; done
while true; do
  if [ -e "$2" ]; then "$dir/flood.sh"; else "$1"; fi
  sleep 0.2
done
