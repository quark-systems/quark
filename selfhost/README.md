# Self-hosting

Quark's own work runs as a Quark Project named "Quark", with `quark-systems/quark` and `quark-systems/firstmate` as its repos.

```sh
cargo run -p quarkd -- --engine firstmate      # in one terminal
selfhost/selfhost.sh --holdout <dir>           # in another
```

`selfhost.sh` creates the Project from [`project.json`](project.json) (or reuses it), waits until it is ready, commits [`project.yaml`](project.yaml) and [`instructions.md`](instructions.md) to its Project repo, and asks the coordinator once to take the open `phase-3` issues.
Running it again only applies what changed.

The verification gates (ADR-15) in `project.yaml`:

| Repo | Checks | Journeys | Holdout |
| :--- | :--- | :--- | :--- |
| `quark` | rustfmt, clippy, `cargo test`, app build and unit tests | the app's Playwright suite against the demo daemon (`npm run e2e:serve`) | `holdout/quark/` when given |
| `firstmate` | `bin/fm-lint.sh` with the pinned shellcheck and actionlint, `bin/fm-test-run.sh --changed` | none | none |

Holdout tests never live in this repo, because every worker on `quark` sees it.
Keep them in a directory outside any worktree, laid out as `<repo>/<category>/run`, and pass it with `--holdout`; they land in the Project repo under `holdout/`.

Standing approval starts off, so Matt merges from the PR center; turn it on in the app to let green PRs merge on their own.
