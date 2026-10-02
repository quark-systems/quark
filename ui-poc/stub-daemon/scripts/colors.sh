#!/usr/bin/env bash
# Streams a coloured agent log with 256-colour and truecolour escapes,
# a progress bar, box drawing, CJK and emoji.
N=$'\e[0m'
tc() { printf '\e[38;2;%d;%d;%dm' "$1" "$2" "$3"; }
c256() { printf '\e[38;5;%dm' "$1"; }
steps=("Indexing repository" "Reading AGENTS.md" "Planning change" "Editing src/store.rs"
  "Running cargo check" "Writing tests" "Running cargo test" "Updating docs/events.md")
notes=("同期完了 — キャッシュを更新しました" "构建成功，耗时 2.3 秒" "검사 통과 ✓" "🚀 deploy preview ready"
  "🧪 11 tests passed" "📦 bundle 412 KiB → 388 KiB" "🔍 found 3 call sites" "✨ formatted 7 files")
i=0
while true; do
  i=$((i + 1))
  printf '%s┌──────────────────────────────────────────────────────────┐%s\n' "$(c256 39)" "$N"
  printf '%s│%s %s◆ quark worker%s  task t-%03d  %s%-26s%s %s│%s\n' "$(c256 39)" "$N" "$(tc 255 140 0)" "$N" "$i" "$(c256 245)" "$(date +%H:%M:%S)" "$N" "$(c256 39)" "$N"
  printf '%s└──────────────────────────────────────────────────────────┘%s\n' "$(c256 39)" "$N"
  for s in "${steps[@]}"; do
    # truecolour gradient progress bar, redrawn in place with \r
    for pct in 0 10 20 30 40 50 60 70 80 90 100; do
      filled=$((pct * 30 / 100))
      bar=""
      for ((k = 0; k < 30; k++)); do
        if ((k < filled)); then
          r=$((40 + k * 7)); g=$((200 - k * 4)); b=$((255 - k * 6))
          bar+="$(tc $r $g $b)█"
        else
          bar+="$(c256 238)░"
        fi
      done
      printf '\r  %s%-24s%s %s%s %3d%%' "$(c256 252)" "$s" "$N" "$bar" "$N" "$pct"
      sleep 0.05
    done
    printf '\r  %s✔%s %-24s %s%s%s\e[K\n' "$(tc 80 220 120)" "$N" "$s" "$(c256 244)" "${notes[RANDOM % ${#notes[@]}]}" "$N"
  done
  # 256-colour swatch row
  for ((k = 16; k < 52; k++)); do printf '\e[48;5;%dm  ' "$k"; done
  printf '%s\n' "$N"
  printf '  %s╭─ summary ─────────────╮%s\n' "$(c256 141)" "$N"
  printf '  %s│%s files changed  %s%3d%s   %s│%s\n' "$(c256 141)" "$N" "$(tc 255 215 0)" $((RANDOM % 9 + 1)) "$N" "$(c256 141)" "$N"
  printf '  %s│%s insertions     %s+%-3d%s  %s│%s\n' "$(c256 141)" "$N" "$(tc 80 220 120)" $((RANDOM % 200)) "$N" "$(c256 141)" "$N"
  printf '  %s│%s deletions      %s-%-3d%s  %s│%s\n' "$(c256 141)" "$N" "$(tc 255 90 90)" $((RANDOM % 60)) "$N" "$(c256 141)" "$N"
  printf '  %s╰───────────────────────╯%s\n\n' "$(c256 141)" "$N"
  sleep 2
done
