#!/usr/bin/env bash
# Set up the Quark Project that builds Quark (Phase 2 self-hosting).
#
# Usage:
#   selfhost/selfhost.sh [--daemon <url>] [--holdout <dir>]
#
# Against a running quarkd (default http://127.0.0.1:7380, or $QUARKD_URL),
# using the firstmate engine:
#   1. creates the Project "Quark" from project.json (quark and firstmate),
#      or reuses it when one with that name exists, and waits until it is
#      ready (a failed one is retried once);
#   2. commits project.yaml (verification gates for both repos) and
#      instructions.md to the Project repo, plus holdout tests copied from
#      --holdout <dir> (laid out as <source>/<category>/run) when given;
#   3. asks the coordinator, once, to take the Phase 3 work items: the open
#      GitHub issues labeled phase-3 in quark-systems/quark.
# Every step is safe to run again. Standing approval stays off; turn it on in
# the app when Quark should merge green PRs without asking.
#
# Holdout tests are kept out of this repo on purpose: workers must never see
# them, and every worker on quark has this directory in its worktree.
#
# Requires curl, jq and git.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DAEMON="${QUARKD_URL:-http://127.0.0.1:7380}"
HOLDOUT=""
KICKOFF_MARK="Phase 3 kickoff"

die() {
  printf 'selfhost: %s\n' "$*" >&2
  exit 1
}
say() { printf 'selfhost: %s\n' "$*"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --daemon) DAEMON=${2:?--daemon needs a URL}; shift 2 ;;
    --holdout) HOLDOUT=${2:?--holdout needs a directory}; shift 2 ;;
    -h|--help) sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done
DAEMON=${DAEMON%/}

for tool in curl jq git; do
  command -v "$tool" >/dev/null || die "$tool is required"
done
if [ -n "$HOLDOUT" ]; then
  [ -d "$HOLDOUT" ] || die "--holdout $HOLDOUT is not a directory"
  HOLDOUT="$(cd "$HOLDOUT" && pwd)"
fi

# api <method> <path> [<json body>]: prints the body; fails on a non-2xx answer.
api() {
  local method=$1 path=$2 body=${3:-} out code
  out=$(mktemp)
  if [ -n "$body" ]; then
    code=$(curl -sS -o "$out" -w '%{http_code}' -X "$method" -H 'content-type: application/json' \
      --data-binary "$body" "$DAEMON$path") || { rm -f "$out"; die "cannot reach quarkd at $DAEMON"; }
  else
    code=$(curl -sS -o "$out" -w '%{http_code}' -X "$method" "$DAEMON$path") \
      || { rm -f "$out"; die "cannot reach quarkd at $DAEMON"; }
  fi
  if [ "${code:0:1}" != 2 ]; then
    printf 'selfhost: %s %s answered %s: %s\n' "$method" "$path" "$code" "$(cat "$out")" >&2
    rm -f "$out"
    exit 1
  fi
  cat "$out"
  rm -f "$out"
}

api GET /v1/health >/dev/null
NAME=$(jq -r .name "$HERE/project.json")

# 1. The Project.
ID=$(api GET /v1/projects | jq -r --arg n "$NAME" '[.[] | select(.name == $n)][0].id // empty')
if [ -n "$ID" ]; then
  # Repos cannot change after creation. A failed Project with other repos
  # (say, https instead of ssh URLs) is renamed out of the way and replaced;
  # a working one is never touched.
  existing=$(api GET "/v1/projects/$ID")
  if ! jq -e --slurpfile want "$HERE/project.json" \
    '[.repos[] | {url, name}] == [$want[0].repos[] | {url, name}]' <<<"$existing" >/dev/null; then
    [ "$(jq -r .status <<<"$existing")" = failed ] \
      || die "Project $NAME ($ID) has other repos than project.json; rename it in the app first"
    old="$NAME (replaced $(date -u +%Y-%m-%dT%H:%M:%SZ))"
    api PATCH "/v1/projects/$ID" "$(jq -n --arg n "$old" '{name: $n}')" >/dev/null
    say "renamed the failed Project $ID with other repos to \"$old\""
    ID=""
  fi
fi
if [ -z "$ID" ]; then
  ID=$(api POST /v1/projects "$(cat "$HERE/project.json")" | jq -r .id)
  say "created Project $NAME ($ID)"
else
  say "using Project $NAME ($ID)"
fi

retried=0
for _ in $(seq 1 300); do
  project=$(api GET "/v1/projects/$ID")
  status=$(jq -r .status <<<"$project")
  case "$status" in
    ready) break ;;
    failed)
      detail=$(jq -r '.status_detail // "no detail"' <<<"$project")
      [ "$retried" = 0 ] || die "provisioning failed again: $detail"
      say "provisioning failed ($detail); retrying once"
      api POST "/v1/projects/$ID:provision" >/dev/null
      retried=1
      ;;
  esac
  sleep 2
done
[ "$status" = ready ] || die "the Project is still $status after 10 minutes: $(jq -r '.status_detail // ""' <<<"$project")"
BARE=$(jq -r '.project_repo_path // empty' <<<"$project")
WORKSPACE=$(jq -r '.workspace_path // empty' <<<"$project")
CREATED=$(jq -r .created_at <<<"$project")
[ -n "$BARE" ] && [ -d "$BARE" ] || die "the Project has no Project repo on this machine"
say "Project ready at $WORKSPACE"

# 2. The Project repo: gates, instructions and holdout tests.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git clone --quiet "$BARE" "$work/project"
# Drop the template's own header comment, then fill in this Project.
sed -e '/^#/d' -e "s|@ID@|$ID|" -e "s|@CREATED_AT@|$CREATED|" "$HERE/project.yaml" >"$work/project/project.yaml"
cp "$HERE/instructions.md" "$work/project/instructions.md"
if [ -n "$HOLDOUT" ]; then
  for src in "$HOLDOUT"/*/; do
    src=$(basename "$src")
    jq -e --arg s "$src" '.repos | any(.name == $s)' "$HERE/project.json" >/dev/null \
      || die "--holdout has $src/, which is not one of the Project's repos"
    rm -rf "$work/project/holdout/$src"
    mkdir -p "$work/project/holdout"
    cp -R "$HOLDOUT/$src" "$work/project/holdout/$src"
  done
  missing=$(find "$work/project/holdout" -mindepth 2 -maxdepth 2 -type d ! -exec test -x '{}/run' ';' -print)
  [ -z "$missing" ] || die "holdout categories without an executable run: $missing"
fi
git -C "$work/project" add --all
if git -C "$work/project" diff --cached --quiet; then
  say "the Project repo is already up to date"
else
  ident=()
  git -C "$work/project" config user.email >/dev/null || ident=(-c user.name=Quark -c user.email=quark@localhost)
  git -C "$work/project" "${ident[@]}" -c commit.gpgsign=false commit --quiet \
    -m "Self-hosting: verification gates, instructions${HOLDOUT:+ and holdout tests}"
  git -C "$work/project" push --quiet origin HEAD:main
  say "committed gates and instructions to the Project repo"
  # The coordinator reads its own checkout; bring it up to date when that is safe.
  if [ -d "$WORKSPACE/project/.git" ]; then
    git -C "$WORKSPACE/project" pull --quiet --ff-only origin main \
      || say "could not fast-forward $WORKSPACE/project; the coordinator will see the change after it pulls"
  fi
fi

# 3. Hand the Phase 3 work items to the coordinator, once. A local marker
# covers the gap before the coordinator's session log shows the message.
marker="${QUARK_HOME:-$HOME/.quark}/selfhost/$ID.kickoff"
if [ -e "$marker" ] || api GET "/v1/coordinators/$ID/messages" | jq -e --arg m "$KICKOFF_MARK" \
  'any(.[]; .text | contains($m))' >/dev/null; then
  say "the coordinator already has the $KICKOFF_MARK"
else
  text=$(jq -n --arg m "$KICKOFF_MARK" '{text: ($m + ": read project/instructions.md again, then list the open GitHub issues labeled phase-3 in quark-systems/quark (gh issue list -R quark-systems/quark -l phase-3). File each one as a task linked to its issue, respecting the dependencies the issues name, and start dispatching. Each issue states its journey and what done means.")}')
  # A coordinator that just started may not have a live terminal yet (503).
  for i in $(seq 1 60); do
    code=$(curl -sS -o "$work/kickoff" -w '%{http_code}' -X POST -H 'content-type: application/json' \
      --data-binary "$text" "$DAEMON/v1/coordinators/$ID/messages") || die "cannot reach quarkd at $DAEMON"
    [ "$code" = 503 ] && [ "$i" -lt 60 ] || break
    sleep 5
  done
  [ "${code:0:1}" = 2 ] || die "the coordinator did not take the kickoff ($code): $(cat "$work/kickoff")"
  mkdir -p "$(dirname "$marker")"
  date -u +%Y-%m-%dT%H:%M:%SZ >"$marker"
  say "sent the $KICKOFF_MARK to the coordinator"
fi
say "done: open the Quark Project in the app to follow along"
