#!/usr/bin/env bash
# Set up a Quark Project from a directory that describes it.
#
# Usage:
#   selfhost/project-init.sh <dir> [--daemon <url>] [--holdout <dir>]
#
# <dir> holds:
#   project.json     the create request (POST /v1/projects): name, goal, repos,
#                    agent_config, dispatch_preset, delivery
#   project.yaml     the Project's project.yaml, verification gates included;
#                    @ID@ and @CREATED_AT@ are filled in from the created
#                    Project and lines starting with # are dropped
#   instructions.md  the coordinator's standing instructions
#   files/           optional; copied as is to the root of the Project repo
#                    (a spec, say)
#   kickoff.md       optional; sent to the coordinator once, as
#                    "<first line>: <the rest>". The first line also marks
#                    whether it was sent, so keep it short and stable.
#
# Against a running quarkd (default http://127.0.0.1:7380, or $QUARKD_URL),
# which should run the native engine:
#   1. creates the Project from project.json, or reuses the one with that
#      name, and waits until it is ready (a failed one is retried once);
#   2. commits project.yaml, instructions.md, files/ and holdout tests copied
#      from --holdout <dir> (laid out as <repo>/<category>/run) to the Project
#      repo;
#   3. sends kickoff.md to the coordinator, once.
# Every step is safe to run again. Standing approval stays off; turn it on in
# the app when Quark should merge green PRs without asking.
#
# Keep holdout tests outside every repo the Project works on: workers must
# never see them.
#
# Requires curl, jq and git.
set -euo pipefail

DAEMON="${QUARKD_URL:-http://127.0.0.1:7380}"
HOLDOUT=""
DIR=""

die() {
  printf 'project-init: %s\n' "$*" >&2
  exit 1
}
say() { printf 'project-init: %s\n' "$*"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --daemon) DAEMON=${2:?--daemon needs a URL}; shift 2 ;;
    --holdout) HOLDOUT=${2:?--holdout needs a directory}; shift 2 ;;
    -h|--help) sed -n '2,34p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) die "unknown argument: $1" ;;
    *) [ -z "$DIR" ] || die "only one directory, got $DIR and $1"; DIR=$1; shift ;;
  esac
done
DAEMON=${DAEMON%/}
[ -n "$DIR" ] || die "usage: project-init.sh <dir> [--daemon <url>] [--holdout <dir>]"
[ -d "$DIR" ] || die "$DIR is not a directory"
DIR="$(cd "$DIR" && pwd)"
for f in project.json project.yaml instructions.md; do
  [ -f "$DIR/$f" ] || die "$DIR has no $f"
done

for tool in curl jq git; do
  command -v "$tool" >/dev/null || die "$tool is required"
done
jq -e '.name | type == "string" and length > 0' "$DIR/project.json" >/dev/null \
  || die "$DIR/project.json has no name"
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
    printf 'project-init: %s %s answered %s: %s\n' "$method" "$path" "$code" "$(cat "$out")" >&2
    rm -f "$out"
    exit 1
  fi
  cat "$out"
  rm -f "$out"
}

ENGINE=$(api GET /v1/health | jq -r '.engine // "unknown"')
[ "$ENGINE" = native ] || say "warning: quarkd runs the $ENGINE engine, not native (quarkd --engine native)"
NAME=$(jq -r .name "$DIR/project.json")

# 1. The Project.
ID=$(api GET /v1/projects | jq -r --arg n "$NAME" '[.[] | select(.name == $n)][0].id // empty')
if [ -n "$ID" ]; then
  # Repos cannot change after creation. A failed Project with other repos
  # (say, https instead of ssh URLs) is renamed out of the way and replaced;
  # a working one is never touched.
  existing=$(api GET "/v1/projects/$ID")
  if ! jq -e --slurpfile want "$DIR/project.json" \
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
  ID=$(api POST /v1/projects "$(cat "$DIR/project.json")" | jq -r .id)
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
  sleep "${PROJECT_INIT_POLL_S:-2}"
done
[ "$status" = ready ] || die "the Project is still $status after 300 checks: $(jq -r '.status_detail // ""' <<<"$project")"
BARE=$(jq -r '.project_repo_path // empty' <<<"$project")
WORKSPACE=$(jq -r '.workspace_path // empty' <<<"$project")
CREATED=$(jq -r .created_at <<<"$project")
[ -n "$BARE" ] && [ -d "$BARE" ] || die "the Project has no Project repo on this machine"
say "Project ready at $WORKSPACE"

# 2. The Project repo: gates, instructions, extra files and holdout tests.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git clone --quiet "$BARE" "$work/project"
# Drop the template's comments, then fill in this Project.
sed -e '/^#/d' -e "s|@ID@|$ID|" -e "s|@CREATED_AT@|$CREATED|" "$DIR/project.yaml" >"$work/project/project.yaml"
cp -f "$DIR/instructions.md" "$work/project/instructions.md"
if [ -d "$DIR/files" ]; then
  cp -Rf "$DIR/files/." "$work/project/"
fi
if [ -n "$HOLDOUT" ]; then
  for src in "$HOLDOUT"/*/; do
    src=$(basename "$src")
    jq -e --arg s "$src" '.repos | any(.name == $s)' "$DIR/project.json" >/dev/null \
      || die "--holdout has $src/, which is not one of the Project's repos"
    rm -rf "$work/project/holdout/$src"
    mkdir -p "$work/project/holdout"
    cp -R "$HOLDOUT/$src" "$work/project/holdout/$src"
  done
  for category in "$work"/project/holdout/*/*/; do
    [ -x "${category}run" ] || die "holdout category ${category#"$work/project/"} has no executable run"
  done
fi
git -C "$work/project" add --all
if git -C "$work/project" diff --cached --quiet; then
  say "the Project repo is already up to date"
else
  # Commit as the person when git knows them; otherwise as Quark, in this
  # throwaway clone only.
  if ! git -C "$work/project" config user.email >/dev/null; then
    git -C "$work/project" config user.name Quark
    git -C "$work/project" config user.email quark@localhost
  fi
  git -C "$work/project" -c commit.gpgsign=false commit --quiet \
    -m "Project setup: verification gates, instructions${HOLDOUT:+ and holdout tests}"
  git -C "$work/project" push --quiet origin HEAD:main
  say "committed gates and instructions to the Project repo"
  # The coordinator reads its own checkout; bring it up to date when that is safe.
  if [ -d "$WORKSPACE/project/.git" ]; then
    git -C "$WORKSPACE/project" pull --quiet --ff-only origin main \
      || say "could not fast-forward $WORKSPACE/project; the coordinator will see the change after it pulls"
  fi
fi

# 3. The kickoff, once. A local marker covers the gap before the
# coordinator's session log shows the message.
if [ -f "$DIR/kickoff.md" ]; then
  mark=$(head -n 1 "$DIR/kickoff.md")
  body=$(tail -n +2 "$DIR/kickoff.md" | tr '\n' ' ' | sed -e 's/^ *//' -e 's/ *$//')
  [ -n "$mark" ] && [ -n "$body" ] || die "kickoff.md needs a short first line and the message after it"
  home="${QUARK_HOME:-$HOME/.quark}"
  slug=$(printf '%s' "$mark" | tr -c 'A-Za-z0-9' '-')
  marker="$home/project-init/$ID.$slug.kickoff"
  if [ -e "$marker" ] || api GET "/v1/coordinators/$ID/messages" | jq -e --arg m "$mark" \
    'any(.[]; .text | contains($m))' >/dev/null; then
    say "the coordinator already has the $mark"
  else
    text=$(jq -n --arg m "$mark" --arg b "$body" '{text: ($m + ": " + $b)}')
    # A coordinator that just started may not have a live terminal yet (503).
    for i in $(seq 1 60); do
      code=$(curl -sS -o "$work/kickoff" -w '%{http_code}' -X POST -H 'content-type: application/json' \
        --data-binary "$text" "$DAEMON/v1/coordinators/$ID/messages") || die "cannot reach quarkd at $DAEMON"
      [ "$code" = 503 ] && [ "$i" -lt 60 ] || break
      sleep "${PROJECT_INIT_RETRY_S:-5}"
    done
    [ "${code:0:1}" = 2 ] || die "the coordinator did not take the kickoff ($code): $(cat "$work/kickoff")"
    mkdir -p "$(dirname "$marker")"
    date -u +%Y-%m-%dT%H:%M:%SZ >"$marker"
    say "sent the $mark to the coordinator"
  fi
fi
say "done: open the $NAME Project in the app to follow along"
