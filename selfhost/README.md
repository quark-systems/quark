# Self-hosting

Quark's own work runs as a Quark Project named "Quark", with `quark-systems/quark` as its repo.

```sh
cargo run -p quarkd -- --engine native         # in one terminal
selfhost/selfhost.sh --holdout <dir>           # in another
```

`selfhost.sh` first installs treehouse 3.1.2 when the machine lacks a 3.x at or above it ([`treehouse.sh`](treehouse.sh); engine slice 9 needs it), then creates the Project from [`project.json`](project.json) (or reuses it), waits until it is ready, commits [`project.yaml`](project.yaml) and [`instructions.md`](instructions.md) to its Project repo, and asks the coordinator once ([`kickoff.md`](kickoff.md)) to take the open `phase-3` issues.
Running it again only applies what changed.
It warns when the daemon runs another engine.

## Any other Project

[`project-init.sh`](project-init.sh) does the same for any Project, from a directory that describes it; `selfhost.sh` is treehouse plus `project-init.sh selfhost/`.

```sh
selfhost/project-init.sh <dir> [--holdout <dir>]
```

`<dir>` holds `project.json` (the create request), `project.yaml` (gates, with `@ID@` and `@CREATED_AT@` filled in), `instructions.md`, optionally `files/` (copied to the Project repo's root, such as a spec) and optionally `kickoff.md` (sent to the coordinator once: a short first line that marks it as sent, then the message).

quarkd runs the native engine ([`docs/engine/native-engine.md`](../docs/engine/native-engine.md)) and starts the PTY supervisor that workers run in (`quarkd ptyd`) itself; it keeps running after quarkd stops, so workers survive a restart.
firstmate, the engine Quark drove before, is retired: quarkd no longer reads a firstmate home, and `~/.quark/engine` (its clone) can be deleted.
A self-hosted Project made before then may still list `firstmate` as a repo; detach it in the Project's settings.

The verification gates (ADR-15) in `project.yaml`:

| Repo | Checks | Journeys | Holdout |
| :--- | :--- | :--- | :--- |
| `quark` | rustfmt, clippy, `cargo test`, app build and unit tests | the app's Playwright suite against the demo daemon (`npm run e2e:serve`) | `holdout/quark/` when given |

Holdout tests never live in this repo, because every worker on `quark` sees it.
Keep them in a directory outside any worktree, laid out as `<repo>/<category>/run`, and pass it with `--holdout`; they land in the Project repo under `holdout/`.

Standing approval starts off, so Matt merges from the PR center; turn it on in the app to let green PRs merge on their own.
