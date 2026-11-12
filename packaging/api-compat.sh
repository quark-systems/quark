#!/usr/bin/env bash
# Compares api/openapi.json with the newest v* tag's (packaging/api-compat.mjs). CI runs it.
set -euo pipefail
cd "$(dirname "$0")/.."
git fetch -q --no-tags origin '+refs/tags/v*:refs/tags/v*' 2>/dev/null || true
tag="$(git -c versionsort.suffix=- tag -l 'v*' --sort=-v:refname | head -1)"
if [ -z "$tag" ]; then echo "no v* tag yet"; exit 0; fi
contract() { sed -n 's/^pub const API_CONTRACT: u32 = \([0-9]*\);/\1/p'; }
tmp="$(mktemp -d)"
git show "$tag:api/openapi.json" > "$tmp/old.json"
old="$(git show "$tag:crates/quark-systems/src/service.rs" | contract)"
new="$(contract < crates/quark-systems/src/service.rs)"
echo "comparing with $tag"
node packaging/api-compat.mjs "$tmp/old.json" api/openapi.json "$old" "$new"
