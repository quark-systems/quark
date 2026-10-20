# Projects

`POST /v1/projects` with `repos` and an `agent_config` provisions a Project in the background (`crates/quarkd/src/provision.rs`).
It clones each repo into the command-center workspace (`~/.quark/workspaces/command`), seeds the Project workspace at `~/.quark/workspaces/<project-id>`, writes the Project repo (`~/.quark/projects/<project-id>.git`, checked out at `<workspace>/project`) and starts the coordinator.

`project.updated` events report each step and the final `ready` or `failed` status; `POST /v1/projects/{id}:provision` retries a failed Project.

The native engine ([`docs/engine/native-engine.md`](../engine/native-engine.md)) runs everything in quarkd; it needs no engine checkout.

[`selfhost/project-init.sh`](../../selfhost/README.md#any-other-project) creates a Project from a directory of files through this API.
