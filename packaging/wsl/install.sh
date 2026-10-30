#!/usr/bin/env bash
# Installs quarkd inside WSL for the Quark desktop app on Windows, and runs it as a systemd
# user service on 127.0.0.1:7380, which WSL forwards to Windows' own loopback. Run it in
# the WSL distro (Ubuntu 24.04 or newer) as your own user:
#
#   curl -fsSL https://github.com/quark-systems/quark/releases/latest/download/install-wsl.sh | bash
#   QUARK_VERSION=0.1.0 bash install-wsl.sh     # a given release instead of the latest
#
# Running it again upgrades quarkd in place. Your repositories, agent CLIs (Claude Code
# and the rest) and their sign-ins live in WSL too: install and sign in to them here.
set -euo pipefail

repo="quark-systems/quark"
version="${QUARK_VERSION:-}"
bin_dir="$HOME/.local/bin"
unit_dir="$HOME/.config/systemd/user"
treehouse_version="3.1.2"
treehouse_sha256="bc059c6dbbcf6b11a741e92aed1d845d5b4a96f1d9c5e6f78c1aa2502669962d"

say() { printf '\033[1m%s\033[0m\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = Linux ] || die "run this inside WSL, not on Windows itself"
[ "$(uname -m)" = x86_64 ] || die "only x86_64 builds of quarkd are published"

# systemd keeps quarkd running without a terminal open. WSL turns it on per distro.
if [ "$(ps -p 1 -o comm= 2>/dev/null)" != systemd ]; then
  die "systemd is off in this distro. Add these lines to /etc/wsl.conf:
    [boot]
    systemd=true
then run 'wsl --shutdown' in PowerShell, reopen WSL and run this again."
fi

say "Checking what quarkd needs"
missing=()
for tool in git tmux curl; do
  command -v "$tool" >/dev/null || missing+=("$tool")
done
if [ ${#missing[@]} -gt 0 ]; then
  say "Installing ${missing[*]} (asks for your WSL password)"
  sudo apt-get update -qq
  sudo apt-get install -y -qq "${missing[@]}"
fi
tmux_version="$(tmux -V | sed 's/[^0-9.]//g')"
if [ "$(printf '%s\n3.2\n' "$tmux_version" | sort -V | head -1)" != 3.2 ]; then
  die "tmux $tmux_version is too old; Quark needs 3.2 or newer"
fi
command -v gh >/dev/null ||
  say "Note: the GitHub CLI (gh) is not installed. Quark needs it signed in: https://cli.github.com, then 'gh auth login'."
command -v claude >/dev/null ||
  say "Note: Claude Code is not installed. The coordinator runs on it: https://docs.claude.com/en/docs/claude-code"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$bin_dir" "$unit_dir"

if [ -n "$version" ]; then
  url="https://github.com/$repo/releases/download/v$version/quarkd-x86_64-unknown-linux-gnu.tar.gz"
else
  url="https://github.com/$repo/releases/latest/download/quarkd-x86_64-unknown-linux-gnu.tar.gz"
fi
say "Downloading quarkd ${version:-(latest)}"
curl -fsSL -o "$tmp/quarkd.tar.gz" "$url"
tar -xzf "$tmp/quarkd.tar.gz" -C "$tmp"
install -m 755 "$tmp/quarkd" "$bin_dir/quarkd"

if ! command -v treehouse >/dev/null && [ ! -x "$bin_dir/treehouse" ]; then
  say "Installing treehouse $treehouse_version (worktrees for workers)"
  curl -fsSL -o "$tmp/treehouse.tgz" \
    "https://github.com/kunchenguid/treehouse/releases/download/v$treehouse_version/treehouse-v$treehouse_version-linux-amd64.tar.gz"
  echo "$treehouse_sha256  $tmp/treehouse.tgz" | sha256sum -c --quiet -
  tar -xzf "$tmp/treehouse.tgz" -C "$tmp" treehouse
  install -m 755 "$tmp/treehouse" "$bin_dir/treehouse"
fi

# The service gets the PATH your login shell has, so quarkd finds gh, tmux and the agent
# CLIs. Run this script again after installing a new one.
login_path="$(bash -lc 'printf %s "$PATH"')"
cat > "$unit_dir/quarkd.service" <<UNIT
[Unit]
Description=Quark daemon (quarkd)
After=network-online.target

[Service]
ExecStart=$bin_dir/quarkd --engine native
Environment=PATH=$bin_dir:$login_path
Restart=on-failure
RestartSec=2
# Workers live in the PTY supervisor quarkd starts; let them outlive a quarkd restart.
KillMode=process

[Install]
WantedBy=default.target
UNIT

say "Starting quarkd"
# Lingering keeps the user service running when no WSL terminal is open.
loginctl enable-linger "$USER" 2>/dev/null || sudo loginctl enable-linger "$USER"
systemctl --user daemon-reload
systemctl --user enable quarkd.service >/dev/null
systemctl --user restart quarkd.service

for _ in $(seq 1 30); do
  if health="$(curl -fsS http://127.0.0.1:7380/v1/health 2>/dev/null)"; then
    say "quarkd is running: $health"
    say "Open Quark on Windows; it connects to http://127.0.0.1:7380."
    exit 0
  fi
  sleep 1
done
die "quarkd did not answer on 127.0.0.1:7380. See: journalctl --user -u quarkd -n 50"
