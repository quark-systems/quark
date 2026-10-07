# Project settings

A user opens a Project's Settings tab and sees every per-Project switch in one place: standing approval, delivery, the agent and its pool, each source's verification gates, and summaries of dispatch rules and memory. Standing approval and each source's holdout tests change there; a holdout change is a commit of `project.yaml` on the Project repo's `main`.

## Sub-features

- `set-read` shows every switch, with the delivery mode and agent read-only.
- `set-standing` toggles standing approval.
- `set-holdout` turns a source's holdout tests off and on, showing the categories found under `holdout/<source>/`.
- `set-links` leads from the dispatch and memory summaries to the screens that edit them.

## How to get to it (user POV)

- From a Project board, the `Settings` button in the header (route `#/p/<project id>/settings`).

## Driving it with quark-verify

Preconditions:

- Mock: `$Q launch --daemon mock`; the `quark` source of `Quark MVP` has one check and the holdout category `daemon-api`.
- Real: `$Q launch` and a ready Project with a local bare repo; push a `holdout/<source>/<category>/run` to the Project repo (`QV_HOME/projects/<id>.git`) to see a category.

- **Open it.** Run `$Q browser open "#/p/<id>"` and `$Q browser click --testid nav-settings`; wait with `$Q browser wait --testid settings-holdout-state --contains Runs`. Save `settings/01-before.png`.
- **Holdout off.** Run `$Q browser click --testid settings-holdout`; `$Q browser wait --testid settings-holdout-state --contains Off`.
- **Standing approval.** Run `$Q browser click --testid standing-approval`. Save `settings/02-after.png`.
- **Side effects.** Run `$Q capture settings/api -- $Q api GET /v1/projects/<id>/settings`: `holdout.enabled` is false and `standing_approval` true. Real: `git -C QV_HOME/projects/<id>.git log -1` is `Change holdout tests` and `project.yaml` on `main` has `holdout: false` under the source.

## Gotchas

- The real daemon rewrites `project.yaml` from its parsed form on a holdout change, so comments in it are lost.
- A source with holdout on but no category directory shows "On, but there are no tests", and no holdout gate runs for it.
