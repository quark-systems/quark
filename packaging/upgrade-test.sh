#!/usr/bin/env bash
# Upgrade and downgrade test: the previous release's quarkd makes a home, this build's
# quarkd must upgrade it and serve, and the previous quarkd must then either still serve
# it (the changes were additive) or refuse it cleanly (it names the newer format or
# schema), never start on it half-way. CI runs it in the rust job:
#   packaging/upgrade-test.sh <this build's quarkd>
# The previous release is the newest v* release other than this version that has a Linux
# quarkd (quarkd-x86_64-unknown-linux-gnu.tar.gz). Needs gh with a token.
set -euo pipefail
new="$(realpath "${1:?usage: packaging/upgrade-test.sh <quarkd>}")"
ours="$("$new" --version | awk '{print $2}')"
work="$(mktemp -d)"
home="$work/home"
port=7391
pids=()
cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  pkill -f "$work" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

prev=""
for tag in $(git -c versionsort.suffix=- tag -l 'v*' --sort=-v:refname); do
  [ "${tag#v}" = "$ours" ] && continue
  if gh release download "$tag" --pattern 'quarkd-x86_64-unknown-linux-gnu.tar.gz' --dir "$work/$tag" 2>/dev/null; then
    prev="$tag"; break
  fi
done
if [ -z "$prev" ]; then
  echo "::warning::no earlier release with a Linux quarkd to upgrade from"
  exit 0
fi
tar -xzf "$work/$prev/quarkd-x86_64-unknown-linux-gnu.tar.gz" -C "$work/$prev"
old="$work/$prev/quarkd"
echo "upgrading from $prev ($("$old" --version)) to $ours"

# Starts quarkd ($1) on the home; prints its health, or returns 1 with its log.
serve() {
  local bin="$1" log="$work/$2.log"
  "$bin" serve --home "$home" --listen "127.0.0.1:$port" >"$log" 2>&1 &
  local pid=$!
  pids+=("$pid")
  for _ in $(seq 1 60); do
    if body="$(curl -fsS "http://127.0.0.1:$port/v1/health" 2>/dev/null)"; then
      echo "$body"
      kill "$pid"; wait "$pid" 2>/dev/null || true
      return 0
    fi
    if ! kill -0 "$pid" 2>/dev/null; then
      cat "$log"; return 1
    fi
    sleep 0.5
  done
  cat "$log"; kill "$pid" || true; return 1
}

echo "--- $prev makes the home"
serve "$old" old-first
echo "--- $ours upgrades it"
health="$(serve "$new" new)"
echo "$health"
grep -q "\"version\":\"$ours\"" <<<"$health"
"$new" backup --home "$home" --label upgrade-test >/dev/null

echo "--- $prev on the upgraded home (a rollback)"
if serve "$old" old-again; then
  echo "$prev still serves the upgraded home"
elif grep -Eq "newer than this quarkd|this build knows" "$work/old-again.log"; then
  echo "$prev refuses the upgraded home cleanly; a rollback restores the backup"
else
  echo "::error::$prev failed on the upgraded home without saying why"
  exit 1
fi
