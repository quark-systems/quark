# Self-hosting

Quark's own work runs as a Quark Project named "Quark", with `quark-systems/quark` and `quark-systems/firstmate` as its repos.

```sh
cargo run -p quarkd -- --engine native         # in one terminal
selfhost/selfhost.sh --holdout <dir>           # in another
```

`selfhost.sh` first installs treehouse 3.1.2 when the machine lacks a 3.x at or above it ([`treehouse.sh`](treehouse.sh); engine slice 9 needs it), then creates the Project from [`project.json`](project.json) (or reuses it), waits until it is ready, commits [`project.yaml`](project.yaml) and [`instructions.md`](instructions.md) to its Project repo, and asks the coordinator once to take the open `phase-3` issues.
Running it again only applies what changed.
It warns when the daemon runs another engine.

## Moving from firstmate to the native engine

The native engine needs no firstmate checkout ([`docs/engine/native-engine.md`](../docs/engine/native-engine.md)).
To move an existing self-hosted Quark over:

1. Let the tasks firstmate is running finish or cancel them: the native engine does not see firstmate's tasks.
2. Stop quarkd, close firstmate's coordinator window for the Project on the `quark` tmux server, and start quarkd again with `--engine native`. The Project, its workspace and its repos stay; coordinator recovery starts a native coordinator for it.
3. Keep `~/.quark/engine` (the firstmate clone) for seven days. If the native engine misbehaves, restart quarkd with `--engine firstmate` to go back.

The verification gates (ADR-15) in `project.yaml`:

| Repo | Checks | Journeys | Holdout |
| :--- | :--- | :--- | :--- |
| `quark` | rustfmt, clippy, `cargo test`, app build and unit tests | the app's Playwright suite against the demo daemon (`npm run e2e:serve`) | `holdout/quark/` when given |
| `firstmate` | `bin/fm-lint.sh` with the pinned shellcheck and actionlint, `bin/fm-test-run.sh --changed` | none | none |

Holdout tests never live in this repo, because every worker on `quark` sees it.
Keep them in a directory outside any worktree, laid out as `<repo>/<category>/run`, and pass it with `--holdout`; they land in the Project repo under `holdout/`.

Standing approval starts off, so Matt merges from the PR center; turn it on in the app to let green PRs merge on their own.
