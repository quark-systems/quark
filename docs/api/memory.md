# Memory

Project memory (`crates/quarkd/src/memory.rs`) turns what tasks learned into reviewable entries.

## How learnings are proposed

A worker reports what a task taught it as a `learned: <text>` status line; `learned [files=a,b]: ...` names the files, and the coordinator writes `learned [source=coordinator]: ...`.
Once the task is in review, done or failed, each line becomes a proposal at `GET /v1/projects/{id}/memory/proposals` with its task, PR and files as evidence (`memory.proposed`).

## Routes

| Route | What it does | Event |
| :--- | :--- | :--- |
| `POST .../memory/proposals/{proposal_id}:accept` | optionally with edited `text`, commits one file under the Project repo's `memory/` in its own commit on `main` and tells the coordinator to read it | `memory.accepted` |
| `POST .../memory/proposals/{proposal_id}:reject` | drops it | `memory.rejected` |
| `GET /v1/projects/{id}/memory` | lists the entries, each with the commit that added it | |
| `GET .../memory/commits/{commit}` | shows that commit | |
| `POST /v1/projects/{id}/memory/{entry_id}:promote` | copies an entry into user-level memory | |
| `GET /v1/memory` | lists user-level memory | |

User-level memory lives in `~/.quark/memory/` (`--user-memory` or `QUARK_USER_MEMORY` picks another directory), and every Project's coordinator is told to read it.
