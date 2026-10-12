#!/usr/bin/env bash
# Set up the Quark Project that builds Quark (Phase 2 self-hosting).
#
# Usage:
#   selfhost/selfhost.sh [--daemon <url>] [--holdout <dir>]
#
# First installs treehouse 3.1.2 when this machine has no 3.x at or above it
# (selfhost/treehouse.sh). Then runs selfhost/project-init.sh on this
# directory, against a running quarkd (default http://127.0.0.1:7380, or
# $QUARKD_URL) that should run the native engine:
#   1. creates the Project "Quark" from project.json (quark and firstmate),
#      or reuses it when one with that name exists, and waits until it is
#      ready (a failed one is retried once);
#   2. commits project.yaml (verification gates for both repos) and
#      instructions.md to the Project repo, plus holdout tests copied from
#      --holdout <dir> (laid out as <source>/<category>/run) when given;
#   3. asks the coordinator, once, to take the Phase 3 work items
#      (kickoff.md): the open GitHub issues labeled phase-3 in
#      quark-systems/quark.
# Every step is safe to run again. Standing approval stays off; turn it on in
# the app when Quark should merge green PRs without asking.
#
# Holdout tests are kept out of this repo on purpose: workers must never see
# them, and every worker on quark has this directory in its worktree.
#
# Requires curl, jq, git and tar.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

case "${1:-}" in
  -h|--help) sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
esac

# Engine slice 9's shadow needs treehouse 3.1.2 or a later 3.x.
"$HERE/treehouse.sh" || { echo "selfhost: treehouse 3.1.2 is required: see selfhost/treehouse.sh --help" >&2; exit 1; }

# Before project-init.sh, the kickoff marker lived under selfhost/; carry it
# over so a Project that already has its kickoff does not get it twice.
home="${QUARK_HOME:-$HOME/.quark}"
for old in "$home"/selfhost/*.kickoff; do
  [ -e "$old" ] || continue
  id=$(basename "$old" .kickoff)
  new="$home/project-init/$id.Phase-3-kickoff.kickoff"
  [ -e "$new" ] || { mkdir -p "$home/project-init"; cp -f "$old" "$new"; }
done

exec "$HERE/project-init.sh" "$HERE" "$@"
