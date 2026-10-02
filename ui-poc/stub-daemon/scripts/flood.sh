#!/usr/bin/env bash
# Stress mode: prints coloured lines as fast as possible until killed.
n=0
while true; do
  n=$((n + 1))
  printf '\e[38;5;%dm%08d\e[0m \e[38;2;%d;%d;%dm████\e[0m stress line — the quick brown fox jumps over the lazy dog 速い 🦊 \e[1m%d\e[0m\n' \
    $((n % 216 + 16)) "$n" $((n * 7 % 256)) $((n * 13 % 256)) $((n * 29 % 256)) "$RANDOM"
done
