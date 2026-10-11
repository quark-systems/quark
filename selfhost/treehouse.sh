#!/usr/bin/env bash
# Make sure treehouse 3.1.2 or a later 3.x is installed.
#
# Usage:
#   selfhost/treehouse.sh [--check] [--dir <bin dir>]
#
# The native worktree pool (engine slice 9) is a port of treehouse 3.1.2, so
# quarkd only compares it with treehouse 3.1.2 or a later 3.x
# (docs/shadow-readiness.md). With such a treehouse first on PATH this prints
# its version and does nothing else. Otherwise it downloads the 3.1.2 release
# for this machine, checks its sha256, and installs it into the directory of
# the treehouse on PATH, or ~/.local/bin when there is none (--dir picks
# another). --check only reports, and exits 1 when an install is needed.
#
# Requires curl and tar.
set -euo pipefail

VERSION=3.1.2
CHECK=0
DIR=""

die() {
  printf 'treehouse: %s\n' "$*" >&2
  exit 1
}
say() { printf 'treehouse: %s\n' "$*"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --check) CHECK=1; shift ;;
    --dir) DIR=${2:?--dir needs a directory}; shift 2 ;;
    -h|--help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

# installed_version <bin>: the first x.y.z that `<bin> --version` prints.
installed_version() {
  "$1" --version 2>/dev/null | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' | head -n 1
}

# new_enough <x.y.z>: 3.x at or above $VERSION.
new_enough() {
  local major minor patch
  IFS=. read -r major minor patch <<<"$1"
  [ "$major" = 3 ] || return 1
  [ "$minor" -gt 1 ] && return 0
  [ "$minor" -eq 1 ] && [ "$patch" -ge 2 ]
}

current=""
if command -v treehouse >/dev/null; then
  current=$(installed_version treehouse || true)
  if [ -n "$current" ] && new_enough "$current"; then
    say "$current is installed at $(command -v treehouse)"
    exit 0
  fi
fi
if [ "$CHECK" = 1 ]; then
  if [ -n "$current" ]; then
    die "$current is installed; $VERSION or a later 3.x is needed (run selfhost/treehouse.sh)"
  fi
  die "not installed; $VERSION or a later 3.x is needed (run selfhost/treehouse.sh)"
fi

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) asset=darwin-arm64 sum=24666ddec346b5fd1f0467f475c647d8e2ce45e80e8a84548eade97b353cbe4b ;;
  Darwin-x86_64) asset=darwin-amd64 sum=19a3a79c792139e36c1f5c3e9e0f9fdebe9127f1168b1debb40b4513ffed1319 ;;
  Linux-x86_64) asset=linux-amd64 sum=bc059c6dbbcf6b11a741e92aed1d845d5b4a96f1d9c5e6f78c1aa2502669962d ;;
  Linux-aarch64|Linux-arm64) asset=linux-arm64 sum=999e6f4888cfd4e6c0b97d49a4857355208bcba080aa2c59902f7fa77b940c0a ;;
  *) die "no treehouse $VERSION release for $(uname -s) $(uname -m)" ;;
esac

if [ -z "$DIR" ]; then
  if command -v treehouse >/dev/null; then
    DIR=$(dirname "$(command -v treehouse)")
  else
    DIR="$HOME/.local/bin"
  fi
fi
mkdir -p "$DIR"
[ -w "$DIR" ] || die "cannot write to $DIR; pass --dir <a directory on PATH you can write>"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
url="https://github.com/kunchenguid/treehouse/releases/download/v$VERSION/treehouse-v$VERSION-$asset.tar.gz"
curl -fsSL -o "$tmp/treehouse.tgz" "$url" || die "cannot download $url"
if command -v sha256sum >/dev/null; then
  got=$(sha256sum "$tmp/treehouse.tgz" | cut -d' ' -f1)
else
  got=$(shasum -a 256 "$tmp/treehouse.tgz" | cut -d' ' -f1)
fi
[ "$got" = "$sum" ] || die "checksum mismatch for $url: got $got"
tar -xzf "$tmp/treehouse.tgz" -C "$tmp" treehouse
chmod 755 "$tmp/treehouse"
mv -f "$tmp/treehouse" "$DIR/treehouse"
say "installed $(installed_version "$DIR/treehouse") at $DIR/treehouse${current:+ (was $current)}"

found=$(command -v treehouse || true)
if [ "$found" != "$DIR/treehouse" ]; then
  say "warning: ${found:-nothing} is first on PATH, not $DIR/treehouse; put $DIR first on PATH"
fi
say "restart quarkd so slice 9's shadow sees it"
