# Create a Project

A user creates a Project from the app with a name, a goal, one or more repositories, a default agent and a dispatch preset; the app lands on the new Project's board while quarkd provisions it, and the Project reads back with what the user entered.

## Sub-features

- `create-validate` refuses a repository that is not owner/name, a clone URL or a local path, and keeps `Create project` disabled.
- `create-agent` offers only installed harnesses that can coordinate, with model and effort.
- `create-submit` creates the Project and lands on its board with a provisioning bar until it is ready.
- `create-provision` provisions the workspace and Project repo and reports each step as `project.updated` events.
- `create-delivery` sets gated or direct delivery under Advanced.

## How to get to it (user POV)

- Choose `New project` in the sidebar (route `#/new`).
- Press `Ctrl+K` and pick `New project` from the palette.
- From the Projects screen when there are none yet.

## Driving it with quark-verify

Preconditions:

- A run launched with `$Q launch` (stub engine) or `$Q launch --engine firstmate --engine-dir <firstmate checkout>`, and `$Q doctor` passes.
- No Project named `Verify demo` (`$Q api GET /v1/projects`).
- With the firstmate engine, a repository it can clone; a local path to a bare repo with a `main` branch works offline (`/path/demo.git`; the form refuses `file://` URLs).

- **Open the form.** Run `$Q browser open "#/new"`. `$Q browser snapshot` shows heading `New project` and a disabled `Create project` button.
- **Refuse a bad repository.** Run `$Q browser fill --label "Repository 1" --value "not a repo"` and `$Q browser wait --text "is not owner/name, a clone URL or a local path"`. `Create project` stays disabled.
- **Fill the form.** Run `$Q browser fill --role textbox --name Name --exact --value "Verify demo"`, `$Q browser fill --role textbox --name Goal --value "Prove verify-quark end to end."`, `$Q browser fill --label "Repository 1" --value "quark-systems/quark"` (stub) or the clone URL (firstmate), `$Q browser select --label Harness --exact --value claude-code` and `$Q browser select --label Effort --exact --value high`. With the firstmate engine and no `no-mistakes`, also `$Q browser click --text Advanced` and `$Q browser select --label Delivery --value direct`. Save `$Q browser screenshot --path create-project/01-form.png`.
- **Create.** Run `$Q browser click --role button --name "Create project"` and `$Q browser wait --testid provision-bar --hidden --timeout 60000`. `$Q browser url` ends in `#/p/prj_...`, `$Q browser text --role heading --name "Verify demo"` prints the name, and the sidebar lists it. Save `create-project/02-board.png`.
- **Read it back.** Run `$Q capture create-project/api -- $Q api GET /v1/projects`. The Project has `status` `ready`, the entered `goal`, the repository normalized to a clone URL with its derived `name`, and `agent_config` `{harness: claude-code, effort: high}`.
- **Side effects.** Run `$Q capture create-project/events -- $Q events --for 1`: `project.updated` events go from `provisioning` to `ready`. Run `$Q capture create-project/repo -- git -C <project_repo_path> log --oneline` with the path from the API: the Project repo has its first commit.

## Gotchas

- `Name` needs `--exact`: the dispatch preset labels also contain the word.
- The harness list comes from what the daemon finds installed; a harness that is not installed is listed but disabled, and Bob is never offered because it cannot coordinate.
- The stub engine reports `ready` without cloning anything and opens no coordinator, so the coordinator chat stays empty; only the firstmate engine proves cloning and the coordinator window.
- A failed provision shows the failing step on the board and in `status_detail`; it is a product failure to report, not a reason to retry through the API.
