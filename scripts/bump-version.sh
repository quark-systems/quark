#!/usr/bin/env bash
# Sets Quark's release version everywhere it is written:
#   scripts/bump-version.sh 0.1.0-beta.1
# quarkd and every workspace crate take it from Cargo.toml; the desktop app keeps its own
# copies (package.json, its Cargo.toml and tauri.conf.json), and QuarkBar's plists are
# filled in from Cargo.toml by mac/scripts/package.sh. crates/quarkd/tests/version.rs
# fails when these drift apart. Tag the release commit `v<version>`.
set -euo pipefail

version="${1:?usage: scripts/bump-version.sh <version>}"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
  echo "not a semver version: $version" >&2
  exit 1
fi
repo="$(cd "$(dirname "$0")/.." && pwd)"

# The first `version = "..."` in each Cargo.toml is the package's (or workspace's) own;
# workspace dependencies on our own crates (`path = "crates/...", version = "..."`) follow it.
for toml in "$repo/Cargo.toml" "$repo/app/src-tauri/Cargo.toml"; do
  awk -v v="$version" '
    !done && /^version = "/ { print "version = \"" v "\""; done = 1; next }
    /path = "crates\/[^"]*", version = "/ { sub(/version = "[^"]*"/, "version = \"" v "\"") }
    { print }' "$toml" > "$toml.tmp" && mv -f "$toml.tmp" "$toml"
done
# The app's JSON files, and its lock file's two copies of the version, rewritten as npm and
# Tauri write them (two-space indent, trailing newline).
node -e '
  const fs = require("fs");
  const [v, ...files] = process.argv.slice(1);
  for (const f of files) {
    const j = JSON.parse(fs.readFileSync(f, "utf8"));
    j.version = v;
    if (j.packages && j.packages[""]) j.packages[""].version = v;
    fs.writeFileSync(f, JSON.stringify(j, null, 2) + "\n");
  }' "$version" "$repo/app/package.json" "$repo/app/package-lock.json" "$repo/app/src-tauri/tauri.conf.json"
# Cargo.lock records the version of our own packages.
cargo update --workspace --offline -q --manifest-path "$repo/Cargo.toml"
cargo update -p quark-desktop --offline -q --manifest-path "$repo/app/src-tauri/Cargo.toml"
echo "Quark is now $version. Commit, then tag v$version."
