# Quark

Quark is an open-source control plane for running teams of coding agents on your own machine.
You give it a goal and the repositories it touches; a coordinator agent plans the work, hands tasks to worker agents in isolated git worktrees, and brings back pull requests that have already passed your checks.
You stay in charge: anything that needs a human (a design question, a red build, a merge) waits for you in one place.

Quark is a Rust daemon (`quarkd`) with a desktop app (Tauri + React) on top. It is MIT licensed.

## Why it exists

Running one agent in one terminal works. Running five across three repos does not: you lose track of who is doing what, questions sit unanswered in a pane you are not looking at, branches collide, and "done" means "the agent said so".
Quark is the layer that makes many agents manageable. It keeps the bookkeeping (worktrees, sessions, accounts, retries, merges) in software, and leaves the judgment to the agents and to you.

## What's good about it

- **Projects, not prompts.** The unit of work is a Project: a goal, a definition of done, its repos, its memory and its issues. One coordinator per Project owns the plan.
- **Evidence-backed pull requests.** Every task runs your repo's checks, Playwright journeys and holdout tests (which workers never see) before a PR opens. Merges rebase onto the latest main and re-verify, and a red main blocks merges until it is fixed.
- **One inbox for you.** Questions from every agent in every Project land in Decisions; PRs, checks and reviews land in the PR center. Next attention (`Ctrl/Cmd+J`) walks whatever waits on you.
- **Harness neutral.** Workers run on Claude Code, Codex, Cursor, Gemini, OpenCode, Pi, Kiro and more, each described by a manifest rather than hard-coded. Dispatch rules route routine tasks to cheaper models and hard ones to frontier models, across several accounts.
- **Durable.** Everything is an event in a crash-safe log. Workers live in a PTY supervisor that outlives the daemon, so a crash, a reboot or a sleep resumes where it stopped.
- **Memory that is reviewable.** What a task learned becomes a proposal you accept or reject; accepted memory is a commit in the Project's own git repo.
- **Local first.** Your code, credentials and agents stay on your machine. Workers can run sandboxed (Seatbelt on macOS, bubblewrap on Linux).

## Why use it

Use Quark if you already get real work out of coding agents and want more of them running at once without babysitting terminals: parallel tasks across repos, PRs you can trust, and a single place to answer questions and merge.
Quark builds itself: its own development runs as a Quark Project.

## Quickstart (from source)

There is no official release yet, so you build it. macOS is the main target; Linux works for the daemon and the web build.

**You need:** Rust (stable), Node 22, git, tmux 3.2+, the [GitHub CLI](https://cli.github.com) signed in (`gh auth login`), and a coordinator harness signed in: [Claude Code](https://docs.claude.com/en/docs/claude-code) (the default), Codex or Pi. Workers can use any supported harness.

```sh
git clone git@github.com:quark-systems/quark.git
cd quark
selfhost/treehouse.sh                     # installs treehouse 3.1.2, which manages worktrees

cargo run --release -p quarkd -- --engine native
```

The daemon listens on `http://127.0.0.1:7380` and keeps its data in `~/.quark` (`QUARK_HOME` moves it). Then, in another terminal:

```sh
cd app && npm install
npm run tauri dev                         # the desktop app
# or: npm run dev, then open http://127.0.0.1:1420 in a browser
```

Click **New project**, give it a name, a goal and a repository, and pick the default agent. The coordinator starts in the Project's Conversation tab; tell it what to do.

Without `--engine native`, quarkd runs an in-memory stub engine meant for tests. To try the app with no agents at all, `npm run mock` in `app/` serves demo data on the same port.

The full walkthrough, with optional pieces (Beads issues, account quota) and where things live on disk, is in [Build from source](docs/getting-started/build-from-source.md).

## Documentation

[`docs/`](docs/README.md) is the documentation index: concepts, the `/v1` API, configuration files, the engine's internals and how to work on Quark itself.

## License

MIT, see [LICENSE](LICENSE). Third-party notices are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
