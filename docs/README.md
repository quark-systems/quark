# Quark documentation

Start with the [README](../README.md) for what Quark is and the quickstart.
Each section below is a folder; add new pages to the folder they belong in and list them here and in that folder's index.

## Getting started

- [Build from source](getting-started/build-from-source.md): prerequisites, building and running quarkd and the app, your first Project, where data lives.
- [External tools](getting-started/external-tools.md): every program Quark runs (quota-axi, treehouse, tmux, gh, bd and more), what for, and how to install it.
- [Releasing](development/releasing.md): cutting a release, the Mac and Windows builds, signing, quarkd in WSL.
- [Self-hosting](../selfhost/README.md): run Quark's own development as a Quark Project, and set up any Project from a directory.

## Concepts

- [Concepts](concepts/README.md): Projects, the coordinator, workers, decisions, verification gates, memory and dispatch, and the names behind them.

## API

The daemon's `/v1` HTTP API and event stream. The [OpenAPI document](../api/openapi.json) is the contract.

- [API overview](api/README.md): conventions, the event stream, regenerating the OpenAPI document.
- [Projects](api/projects.md): creating and provisioning Projects.
- [Decisions](api/decisions.md): questions and holds that wait on a person.
- [Pull requests](api/pull-requests.md): the PR center, gate evidence, comments and merges.
- [Terminals](api/terminals.md): coordinator and worker terminals.
- [Memory](api/memory.md): proposals, Project memory and user-level memory.
- [Dispatch](api/dispatch.md): reading, saving and testing dispatch rules.
- [Imports](api/imports.md): seeding Quark from a firstmate home.
- [Desktop app contract](../app/CONTRACT.md): every endpoint the app calls and the shape it expects.

## Configuration

- [Verification gates](configuration/verification-gates.md): checks, journeys and holdout tests in `project.yaml`.
- [Dispatch rules](configuration/dispatch-rules.md): `dispatch.yaml`, the classifier and `~/.quark/config.yaml`.
- [Importing from firstmate](configuration/firstmate-import.md): what an existing firstmate home becomes in Quark.

## Engine internals

How `quarkd --engine native` works inside. See the [engine index](engine/README.md).

## Development

- [Working on Quark](development/README.md): repository layout, crates, tests, CI and agent skills.
- [Desktop app](../app/README.md): screens, keys, running and testing the app.
- [App glossary](../app/GLOSSARY.md): the words the app uses.
- [UI proof of concept](../ui-poc/FINDINGS.md): the Phase 0 benchmarks behind the Tauri + xterm.js choice.
