# Persona packs

A user picks the persona a Project reads with, beside its coordinator chat; the role names and a few labels on the board, the chat and the worker view change at once, while the API and events keep the neutral names.

## Sub-features

- `persona-pick` sets a Project's own pack (`persona-picker` in the chat head) or clears it with "Default (...)".
- `persona-labels` relabels the coordinator button and chat, the "Needs decision" column, the Memory button and the worker's message box.
- `persona-default` changes the global default (`PUT /v1/personas/default`); Projects without their own pack follow it.
- `persona-user-pack` serves a pack dropped under `<home>/personas/<id>/pack.toml` with no release.

## How to get to it (user POV)

- Open a Project (route `#/p/<project>`) and use the Persona select in the coordinator chat's header.
- Or open the Project's Settings tab (route `#/p/<project>/settings`), Persona section.
- The global default has no screen yet; it is set through the API.

## Driving it with quark-verify

Preconditions:

- Mock: `$Q launch --daemon mock`; its default pack is `plain`, so the demo starts with neutral names.
- Real: `$Q launch` with any engine and a Project; the default pack is `nautical` unless `QV_HOME/personas.toml` says otherwise.

- **Read the packs.** `$Q api GET /v1/personas` lists `plain`, `nautical` and `kitchen-brigade` and the default.
- **Switch.** Run `$Q browser open "#/p/quark"`, save `personas/01-before.png`, then `$Q browser select --testid persona-picker --value kitchen-brigade` and `$Q browser wait --label "Message the expo"`. Save `personas/02-after.png`.
- **Side effects.** `$Q api GET /v1/projects/quark/persona` shows `project_override: "kitchen-brigade"`; `QV_HOME/personas.toml` (real) holds the choice; `$Q api GET /v1/projects/quark` has no persona words.
- **Clear.** Select "Default (...)" (`--value ""`) and wait for the default pack's labels.

## Gotchas

- Labels load once per Project per app session; a change made through the API from elsewhere shows after a reload.
- Engineering objects (pull requests, checks, merges) and task states other than "Needs decision" are never relabeled.
