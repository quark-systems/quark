# Changelog

Quark's notable changes, newest first. Versions follow [SemVer](https://semver.org); while
Quark is on 0.x, a minor version may change the `/v1` API or the `~/.quark` format, and
quarkd upgrades an older home on its first start (see
[Upgrading](docs/getting-started/build-from-source.md#upgrading)). Each release's GitHub
page lists every merged pull request.

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
  Pi). It installs what it can with one button (Homebrew, npm) and gives the command for
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
- **Issues and memory** on Beads (optional: needs bd and Dolt), with GitHub sync.
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
