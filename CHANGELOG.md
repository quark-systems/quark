# Changelog

Quark's notable changes, newest first. Versions follow [SemVer](https://semver.org); while
Quark is on 0.x, a minor version may change the `/v1` API or the `~/.quark` format, and
quarkd upgrades an older home on its first start (see
[Upgrading](docs/getting-started/build-from-source.md#upgrading)). Each release's GitHub
page lists every merged pull request.

## Unreleased

### Added

- Quark reads quota for OpenRouter, DeepSeek, Z.ai, MiniMax, OpenCode Go, Command Code and
  Xiaomi MiMo (with their API keys) and Alibaba Coding Plan (with `bl`). Pi and OpenCode
  accounts show a row for every provider they are signed in to.

### Fixed

- After an update is undone because the new version didn't start, the updater's log names
  the version that is back in place, not the one that failed.
- The health-check error now says the new quarkd didn't stay up when it answered and then
  died, instead of saying it never answered.

## 0.2.0-beta.2

A beta to test updating from 0.2.0-beta.1: Quark on the beta channel offers it on its own.

### Changed

- quarkd's log records each update check (its result and any offer), each download, and
  each install or rollback it starts.

## 0.2.0-beta.1

The first build that updates itself. A pre-release: it reaches people on the beta channel,
and is how the update path from here on gets tested before 0.2.0.

### Install

Same as 0.1.0: download `Quark_0.2.0-beta.1_aarch64.dmg`, drag Quark to Applications, and
run `xattr -dr com.apple.quarantine /Applications/Quark.app` before the first launch. Quark
0.1.0 doesn't offer this build (it only follows full releases), so install it by hand once;
from here on Quark updates itself.

### Added

- Automatic updates. quarkd checks for a new Quark every six hours and on Check for
  Updates… (in the Quark menu and the menu bar), downloads it and verifies its signature.
  Install and restart swaps Quark.app, restarts quarkd and reopens the apps; running
  workers carry on. If the new version doesn't start within two minutes, the old one comes
  back by itself, and Roll back returns to it by hand. Updates (in the sidebar) picks the channel
  (stable or beta) and whether to download on its own.
- quarkd backs up its databases before an update, and `quarkd backup` takes one any time.
- The app and menu bar say when quarkd is too new or too old for them, by API contract
  instead of version number.

### Changed

- Releases publish themselves once their downloads check out, and the update feed follows.
- The desktop app no longer runs its own updater; quarkd updates the whole bundle.

## 0.1.0

The first stable Quark: the Mac app for Apple silicon, still unsigned (see Install). It is
alpha.3 plus the fixes below, which came out of testing a clean install in a macOS VM.

### Install

Download `Quark_0.1.0_aarch64.dmg` and drag Quark to Applications. The build is not signed
or notarized yet, so before the first launch run
`xattr -dr com.apple.quarantine /Applications/Quark.app` (or right-click Quark and choose
Open). Opening Quark starts its menu bar app and quarkd, and Setup lists anything missing.
Upgrading from an alpha: quit Quark, drag the new one over it, clear the quarantine flag the
same way, open Quark, and choose Restart quarkd in the menu bar so the new quarkd runs.

### Fixed

- Opening a newly installed Quark starts quarkd and puts QuarkBar in Login Items, also on a
  Mac that ran Quark from source before or where another quarkd was answering. QuarkBar
  remembered that setup was done for any build, so it often never turned quarkd on. Now each
  new build sets itself up once, or at the next login if another quarkd was running; Stop
  quarkd still stays stopped until the next upgrade.
- Creating a project on a Mac that never connected to GitHub over SSH failed with "Host key
  verification failed". Setup has a new **SSH to GitHub** check: it adds GitHub's published host
  keys to `~/.ssh/known_hosts` with one button, or says when GitHub doesn't know this Mac's SSH
  key. A clone that fails this way points at the check.
- Quark notifications show the Quark icon, not the old placeholder.
- A notice about a task links to that task.
- The coordinator is no longer woken to pick an agent it was already asked to pick.

### Changed

- Quark reads Claude Code and Codex quota itself; `quota-axi` is no longer needed and Setup no
  longer lists it. The Claude read works when quarkd is started by QuarkBar at login, without
  a Keychain prompt. Codex quota also comes through Pi's Codex sign-in when Codex itself is not
  signed in. `quarkd quota claude` and `quarkd quota codex` print a reading.
- Quark also reads quota itself for Grok, Kimi Code, Cursor, Antigravity and Muse (which
  quota-axi covered) and, new, Gemini CLI and GitHub Copilot. Accounts shows their quota and
  dispatch ranks them; `quarkd quota <provider>` prints a reading. Cursor's editor sign-in is
  read with `sqlite3`.
- A permission prompt on a worker's screen reaches the coordinator as a worker question it
  can answer.
- After a merge, a Project's local clones move to origin's default branch.

## 0.1.0-alpha.3

Installed the same way as alpha.2. After dragging Quark over the old one, run
`xattr -dr com.apple.quarantine /Applications/Quark.app`, open Quark, and choose Restart
quarkd in the menu bar so the new quarkd runs.

### Fixed

- Coordinator chats and worker transcripts open on their newest entries. Before, a chat
  longer than 1,000 messages hid everything newer after the app reloaded.
- The app reads quarkd's version again when it reconnects, so it shows the right one after
  quarkd restarts or upgrades.

### Changed

- The native worktree pool seeds files listed in `.worktreeinclude` and closes the other
  gaps it had with treehouse.
- IBM Bob workers get Quark's worker hooks, and their brief is typed once, after Bob has
  started; a task can report a second pull request.
- Harness API keys for default logins (Kiro too) are kept in the Keychain.
- New Quark icons, and a docs diagram of how quarkd works.

## 0.1.0-alpha.2

A fix build of the first alpha, installed the same way (download
`Quark_0.1.0-alpha.2_aarch64.dmg`, drag Quark to Applications, then right-click and Open,
or run `xattr -dr com.apple.quarantine /Applications/Quark.app` before the first launch so
Quark can start its menu bar app and quarkd).

### Fixed

- Setup and `quarkd doctor` no longer list treehouse unless `QUARK_WORKTREE_POOL` asks for
  it, and they read versions written as `v3.1.5` correctly.
- Terminal text no longer garbles: xterm.js moves to the 6.1 beta with the WebGL atlas fix.
- The README and docs describe installing the Mac app and the native engine as the
  default.

## 0.1.0-alpha.1

The first tagged build of Quark: a Mac app you install instead of building from source.
It is an **alpha**: Apple silicon only, not signed or notarized, and without automatic
updates.

### Install

- Download `Quark_0.1.0-alpha.1_aarch64.dmg` from the release and drag Quark to
  Applications. The build is ad-hoc signed, so macOS quarantines it the first time:
  right-click Quark and choose Open, or run
  `xattr -dr com.apple.quarantine /Applications/Quark.app`.
- Opening Quark starts the menu bar app (QuarkBar), which registers quarkd as a login
  item, so all three come back after a restart. quarkd logs to
  `~/Library/Logs/Quark/quarkd.log`.
- The **Setup** page opens on first run and lists what this Mac still needs: git, tmux
  3.2+, the GitHub CLI signed in, a coordinator harness signed in (Claude Code, Codex or
  Pi), and Beads (bd and Dolt). It installs what it can with one button (Homebrew, npm) and gives the command for
  sign-ins. `quarkd doctor` runs the
  same checks in a terminal.

### What's in it

- **Projects run on quarkd's own engine**, now the default (`--engine stub` remains for
  tests). Each project has a coordinator (Claude Code, Codex or Pi) that plans, starts
  workers in their own worktrees, asks you decisions and merges. Worktrees come from
  Quark's own pool, so treehouse is no longer needed (`QUARK_WORKTREE_POOL=treehouse`
  falls back to it).
- **Workers** on Claude Code, Codex, Cursor Agent, Gemini CLI, OpenCode, Pi, IBM Bob, Kiro
  CLI and more, picked by per-project dispatch rules with quota-aware account pools and
  an optional classifier (System One or Ollama). Workers survive quarkd restarts in the
  PTY supervisor; the fleet check nudges quiet ones and relaunches stuck ones.
- **Decisions**: every question waits in one inbox with options and a recommendation;
  standing rules let the coordinator decide matching cases and log them.
- **PR center**: checks, reviews and changes for every worker PR; guarded merges rebase
  first, and workers hear about conflicts and failed checks.
- **Issues and memory** on Beads (bd and Dolt), with GitHub sync.
- **Project dashboard**: Overview, Metrics (spend, tokens, outcomes) and Settings,
  including the coordinator's context meter, compaction and agent changes with a handoff.
- **Automation**: inbox, triggers and an away policy.
- **Hosts and Accounts**: host health and worktree pools; harness accounts with live
  quota, including Bob coins and Kiro credits.
- **QuarkBar**: menu bar status, notifications when something needs you, and desktop
  widgets (Needs you, Projects, Quota, Hosts).
- **Import from firstmate**: bring firstmate projects, backlog, holds and memory over;
  the firstmate folder is only read.
- **Diagnostics**: `quarkd diagnose` and `quarkd triage` report and file what went wrong
  lately.
- **Bounded data**: housekeeping prunes old host telemetry and retired events from the
  event log, and old notices and finished workers' terminal output from the store, so
  `~/.quark` stops growing without limit.

### Upgrading from a source build

- `~/.quark` now has a format version (`format.json`). The first start of this release
  upgrades a home made by an earlier source build: firstmate's files are moved out of
  project workspaces into `~/.quark/backups/firstmate`, and source clones firstmate made
  under a workspace's `projects/` are linked from `repos/`. A quarkd older than the home
  refuses to start instead of reading it.
- quarkd no longer needs `--engine native`.
- If you ran a separate `~/Applications/QuarkBar.app`, quit and delete it; Quark.app
  carries its own.

### Known limitations

- Apple silicon Macs only; no Windows or Intel build in 0.1.
- Not signed or notarized, and no automatic updates yet: install the next build by hand.
- Account quota for Claude and Codex needs `quota-axi` (optional).
