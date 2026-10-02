//! Seed data: projects, tasks, decisions, pull requests, comments and chat
//! histories, plus the pools background activity draws from.

use std::collections::HashMap;

use crate::model::*;

pub const DIFFS: [&str; 4] = [
    include_str!("../diffs/pr1.diff"),
    include_str!("../diffs/pr2.diff"),
    include_str!("../diffs/pr3.diff"),
    include_str!("../diffs/pr4.diff"),
];

/// Mutable domain state behind the REST API.
pub struct Data {
    pub projects: Vec<(String, String, String)>, // (id, name, repo)
    pub tasks: Vec<Task>,
    pub decisions: Vec<Decision>,
    pub prs: Vec<PullRequest>,
    pub diffs: HashMap<String, &'static str>,
    pub comments: HashMap<String, Vec<Comment>>,
    pub chats: HashMap<String, Vec<ChatMessage>>,
    next_id: u64,
}

impl Data {
    /// Fresh id with a prefix, e.g. `t-142`, `c-981`.
    pub fn next_id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{}", self.next_id)
    }

    pub fn project_list(&self) -> Vec<Project> {
        self.projects
            .iter()
            .map(|(id, name, repo)| Project {
                id: id.clone(),
                name: name.clone(),
                repo: repo.clone(),
                active_tasks: self
                    .tasks
                    .iter()
                    .filter(|t| {
                        &t.project_id == id
                            && !matches!(t.state, TaskState::Done | TaskState::Failed)
                    })
                    .count(),
            })
            .collect()
    }

    pub fn task_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }
}

fn opt(label: &str, consequence: &str) -> DecisionOption {
    DecisionOption {
        label: label.into(),
        consequence: consequence.into(),
    }
}

fn task(id: &str, project: &str, title: &str, state: TaskState, harness: &str, ago: i64) -> Task {
    let slug: String = title
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("-");
    Task {
        id: id.into(),
        project_id: project.into(),
        title: title.into(),
        state,
        harness: harness.into(),
        branch: format!("fm/{id}-{slug}"),
        updated_at: ts_ago(ago),
    }
}

pub fn make_task(id: &str, project: &str, title: &str, harness: &str) -> Task {
    task(id, project, title, TaskState::Queued, harness, 0)
}

/// Unified diff line counts: (additions, deletions).
fn diff_stats(diff: &str) -> (u32, u32) {
    let mut add = 0;
    let mut del = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            add += 1;
        } else if line.starts_with('-') {
            del += 1;
        }
    }
    (add, del)
}

/// Worker panes are bound to these tasks (all running, never auto-transitioned).
pub const WORKER_TASKS: [(&str, &str); 4] = [
    ("t-101", "cargo test"),
    ("t-102", "top"),
    ("t-201", "agent log"),
    ("t-103", "shell"),
];

pub fn seed() -> Data {
    use TaskState::*;
    let projects = vec![
        ("quark", "Quark", "github.com/quark-systems/quark"),
        (
            "firstmate",
            "Firstmate",
            "github.com/quark-systems/firstmate",
        ),
        ("website", "Website", "github.com/quark-systems/website"),
    ]
    .into_iter()
    .map(|(a, b, c)| (a.into(), b.into(), c.into()))
    .collect();

    let tasks = vec![
        task(
            "t-101",
            "quark",
            "Bounded event store with per-worker retention",
            Running,
            "claude",
            40,
        ),
        task(
            "t-102",
            "quark",
            "Profile quarkd memory under 8 workers",
            Running,
            "codex",
            95,
        ),
        task(
            "t-103",
            "quark",
            "Interactive shell latency harness",
            Running,
            "claude",
            12,
        ),
        task(
            "t-104",
            "quark",
            "Add backpressure to event fan-out",
            Review,
            "claude",
            600,
        ),
        task(
            "t-105",
            "quark",
            "Worker pane resize handling",
            Review,
            "codex",
            1500,
        ),
        task(
            "t-106",
            "quark",
            "Persist decisions across daemon restarts",
            NeedsDecision,
            "claude",
            300,
        ),
        task(
            "t-107",
            "quark",
            "Migrate config loader to figment",
            Queued,
            "pi",
            3600,
        ),
        task(
            "t-108",
            "quark",
            "Fix flaky tmux attach test on CI",
            Failed,
            "codex",
            7200,
        ),
        task(
            "t-201",
            "firstmate",
            "Typed decision records",
            Running,
            "claude",
            30,
        ),
        task(
            "t-202",
            "firstmate",
            "Reconcile backlog after secondmate restart",
            NeedsDecision,
            "codex",
            420,
        ),
        task(
            "t-203",
            "firstmate",
            "Shellcheck sweep of bin/ helpers",
            Done,
            "claude",
            86400,
        ),
        task(
            "t-204",
            "firstmate",
            "Watcher heartbeat jitter",
            Queued,
            "opencode",
            5000,
        ),
        task(
            "t-301",
            "website",
            "Pricing page and plan cards",
            Review,
            "claude",
            900,
        ),
        task(
            "t-302",
            "website",
            "Changelog RSS feed",
            NeedsDecision,
            "codex",
            200,
        ),
        task(
            "t-303",
            "website",
            "Lighthouse pass on landing page",
            Done,
            "claude",
            172800,
        ),
    ];

    let decisions = vec![
        Decision {
            id: "d-1".into(),
            project_id: "quark".into(),
            task_id: "t-106".into(),
            question: "Where should answered decisions be persisted?".into(),
            context: "The daemon currently keeps decisions in memory, so a restart loses \
                      every open question. Two storage options fit the existing code; the \
                      SQLite path adds a dependency but gives us queries for the history view."
                .into(),
            options: vec![
                opt(
                    "SQLite via rusqlite",
                    "Adds ~1.2 MB to the binary; enables history queries",
                ),
                opt(
                    "Append-only JSONL",
                    "No new deps; history view needs a full scan",
                ),
                opt(
                    "Defer until v0.5",
                    "Restarts keep losing open decisions for now",
                ),
            ],
            recommended: 0,
            state: DecisionState::Open,
            answer: None,
        },
        Decision {
            id: "d-2".into(),
            project_id: "firstmate".into(),
            task_id: "t-202".into(),
            question: "Two backlog entries claim the same worktree. Which one wins?".into(),
            context: "After the secondmate restarted, `fm/t-198-watch-jitter` and \
                      `fm/t-202-reconcile-backlog` both point at the same worktree. \
                      t-198 has 3 unpushed commits; t-202 has none."
                .into(),
            options: vec![
                opt("Keep t-198", "t-202 is re-queued with a fresh worktree"),
                opt(
                    "Keep t-202",
                    "t-198's 3 unpushed commits move to a rescue branch",
                ),
            ],
            recommended: 0,
            state: DecisionState::Open,
            answer: None,
        },
        Decision {
            id: "d-3".into(),
            project_id: "website".into(),
            task_id: "t-302".into(),
            question: "Which feed format should the changelog publish?".into(),
            context: "The static site generator supports both. Most readers we checked \
                      handle either; Atom has stricter date semantics."
                .into(),
            options: vec![
                opt("RSS 2.0", "Widest reader support; loose date format"),
                opt("Atom 1.0", "Strict timestamps; slightly less common"),
                opt("Both", "Two feeds to keep in sync; ~40 extra lines"),
                opt("JSON Feed", "Modern, tiny; few readers support it"),
            ],
            recommended: 1,
            state: DecisionState::Open,
            answer: None,
        },
        Decision {
            id: "d-0".into(),
            project_id: "quark".into(),
            task_id: "t-108".into(),
            question: "Retry the flaky tmux attach test or quarantine it?".into(),
            context: "Failed 3 of the last 20 CI runs, always on the macOS runner.".into(),
            options: vec![
                opt("Quarantine", "CI goes green; the race stays unfixed"),
                opt("Retry 3x", "Masks the race; CI time +40s on failure"),
            ],
            recommended: 0,
            state: DecisionState::Answered,
            answer: Some(0),
        },
    ];

    let pr_meta = [
        (
            "pr-1",
            "quark",
            "t-104",
            412,
            "Add backpressure to event fan-out",
            PrState::Open,
            Checks::Passing,
            "medium",
        ),
        (
            "pr-2",
            "quark",
            "t-105",
            415,
            "Worker pane resize handling",
            PrState::Open,
            Checks::Pending,
            "low",
        ),
        (
            "pr-3",
            "firstmate",
            "t-201",
            88,
            "Typed decision records",
            PrState::Draft,
            Checks::Failing,
            "medium",
        ),
        (
            "pr-4",
            "website",
            "t-301",
            57,
            "Pricing page and plan cards",
            PrState::Open,
            Checks::Passing,
            "low",
        ),
    ];
    let mut prs = Vec::new();
    let mut diffs = HashMap::new();
    for (i, (id, project, task_id, number, title, state, checks, risk)) in
        pr_meta.into_iter().enumerate()
    {
        let (additions, deletions) = diff_stats(DIFFS[i]);
        diffs.insert(id.to_string(), DIFFS[i]);
        prs.push(PullRequest {
            id: id.into(),
            project_id: project.into(),
            task_id: task_id.into(),
            number,
            title: title.into(),
            url: format!("https://github.com/quark-systems/{project}/pull/{number}"),
            state,
            checks,
            additions,
            deletions,
            risk: risk.into(),
        });
    }

    let c = |id: &str, path: &str, line: u32, body: &str, author: &str, ago: i64| Comment {
        id: id.into(),
        path: path.into(),
        line,
        body: body.into(),
        author: author.into(),
        ts: ts_ago(ago),
    };
    let mut comments = HashMap::new();
    comments.insert(
        "pr-1".to_string(),
        vec![
            c("c-1", "crates/quarkd/src/events.rs", 45, "Holding the store lock across `send` is deliberate, right? Worth a comment that `send` never awaits.", "captain", 1800),
            c("c-2", "crates/quarkd/src/events.rs", 45, "Yes: `broadcast::Sender::send` is synchronous, so the lock is held for microseconds. Added a doc line above.", "claude", 1700),
            c("c-3", "ui/src/hooks/useEvents.ts", 31, "Should backoff reset only after the first message rather than on open?", "captain", 900),
        ],
    );
    comments.insert(
        "pr-2".to_string(),
        vec![c(
            "c-4",
            "crates/quarkd/src/workers/tmux.rs",
            51,
            "Is 20x5 too strict for the mobile layout?",
            "captain",
            2400,
        )],
    );
    comments.insert(
        "pr-3".to_string(),
        vec![c(
            "c-5",
            "src/decision.rs",
            41,
            "CI fails because `thiserror` is not in Cargo.toml yet.",
            "codex",
            600,
        )],
    );
    comments.insert("pr-4".to_string(), Vec::new());

    let m = |id: &str, role: Role, text: &str, ago: i64| ChatMessage {
        id: id.into(),
        role,
        text: text.into(),
        ts: ts_ago(ago),
    };
    use Role::{Coordinator as Co, User as Us};
    let mut chats = HashMap::new();
    chats.insert(
        "quark".to_string(),
        vec![
            m("m-1", Us, "What's the state of the event store work?", 3600),
            m("m-2", Co, QUARK_HISTORY_1, 3590),
            m(
                "m-3",
                Us,
                "Good. Can you show me how a client should resume?",
                3000,
            ),
            m("m-4", Co, QUARK_HISTORY_2, 2990),
        ],
    );
    chats.insert(
        "firstmate".to_string(),
        vec![
            m(
                "m-5",
                Us,
                "Why is the backlog reconcile task waiting on me?",
                1200,
            ),
            m("m-6", Co, FIRSTMATE_HISTORY_1, 1190),
        ],
    );
    chats.insert(
        "website".to_string(),
        vec![
            m(
                "m-7",
                Us,
                "Ship the pricing page today if checks are green.",
                5400,
            ),
            m("m-8", Co, WEBSITE_HISTORY_1, 5390),
        ],
    );

    Data {
        projects,
        tasks,
        decisions,
        prs,
        diffs,
        comments,
        chats,
        next_id: 1000,
    }
}

const QUARK_HISTORY_1: &str = r#"Here's where the **event store** work stands:

1. `EventStore` keeps a ring of events with a monotonic `seq`.
2. `worker.output` is bounded to **2 MB per worker**; everything else keeps the last 20k events.
3. Slow clients are *lagged* off the broadcast channel and resync from the store.

Open items:

- [x] Retention test for worker output
- [ ] Metrics for lagged clients
- [ ] Docs for `?cursor=`

PR #412 is green and waiting for review."#;

const QUARK_HISTORY_2: &str = r#"Sure. Remember the last `seq` you processed and reconnect with it as the cursor:

```ts
let cursor = 0;
function connect() {
  const ws = new WebSocket(`ws://127.0.0.1:7420/v1/events?cursor=${cursor}`);
  ws.onmessage = (m) => {
    const ev = JSON.parse(m.data);
    if (ev.seq <= cursor) return; // duplicate after resync
    cursor = ev.seq;
    handle(ev);
  };
  ws.onclose = () => setTimeout(connect, 500);
}
```

The server replays everything retained with `seq > cursor`, then streams live."#;

const FIRSTMATE_HISTORY_1: &str = r#"Two backlog entries ended up pointing at **the same worktree** after the secondmate restarted:

| Task | Branch | Unpushed commits |
| --- | --- | --- |
| t-198 | `fm/t-198-watch-jitter` | 3 |
| t-202 | `fm/t-202-reconcile-backlog` | 0 |

I can't pick one without risking those 3 commits, so I opened decision **d-2**. My recommendation is to keep t-198 and re-queue t-202 with a fresh worktree."#;

const WEBSITE_HISTORY_1: &str = r#"Aye. PR #57 (*Pricing page and plan cards*) has **passing checks**. Summary:

- New `PlanCard` component with a highlighted Team plan
- `formatPrice()` handles free and "contact us" plans
- Changelog entry for **0.4.0**

One thing to confirm before merge: the Team price moves from `$20` to `$24` per seat. Existing teams keep their price until renewal."#;

/// Canned coordinator replies streamed in response to chat messages.
pub const REPLIES: [&str; 4] = [
    r#"Understood. Here's the plan:

1. **Reproduce** the issue on a clean worktree.
2. Add a failing test in `crates/quarkd/tests/events.rs`.
3. Fix it and run the full suite.

```rust
#[tokio::test]
async fn replay_after_eviction_is_contiguous() {
    let hub = Hub::new();
    for i in 0..10_000 {
        hub.publish("quark", "task.created", &i);
    }
    let events = hub.since(9_990);
    assert_eq!(events.len(), 10);
    assert!(events.windows(2).all(|w| w[0].seq + 1 == w[1].seq));
}
```

I'll dispatch a worker now and report back when the PR is up."#,
    r#"Here's a quick comparison of the options:

| Option | Latency | Memory | Effort |
| --- | ---: | ---: | --- |
| Broadcast + store resync | ~1 ms | bounded | **low** |
| Per-client mpsc queues | ~1 ms | unbounded | medium |
| Polling `/v1/events` | 250 ms | none | low |

My recommendation is the **broadcast + store resync** approach: it keeps memory bounded and never blocks the publisher. The trade-off is that a very slow client may see a gap if its missed events were already evicted, so it should refetch snapshots over REST.

Want me to go ahead?"#,
    r#"Checked the fleet:

- **3 workers** are running (`t-101`, `t-102`, `t-201`)
- **1 PR** is waiting for your review: #415
- `t-108` failed on CI; logs point at a race in `tmux attach`

To reproduce locally:

```bash
cargo test -p quarkd tmux::attach -- --nocapture --test-threads=1
```

Nothing else needs you right now."#,
    r#"Good question. The daemon forwards keystrokes as raw bytes using `send-keys -H`, so control sequences survive intact:

```text
POST /v1/workers/w-4/input  {"data_b64": "bHMgLWxhDQ=="}
  -> send-keys -t %3 -H 6c 73 20 2d 6c 61 0d
```

A few notes:

- *Arrow keys* arrive as `ESC [ A` and friends
- **Bracketed paste** works because nothing is re-quoted
- Output comes back as `worker.output` events with base64 `data_b64`

> Latency is dominated by the round-trip through tmux, typically under 5 ms locally."#,
];

/// Pool for background task creation.
pub const NEW_TASKS: [(&str, &str, &str); 10] = [
    ("quark", "Expose lagged-client metric", "claude"),
    (
        "quark",
        "Graceful shutdown drains websocket clients",
        "codex",
    ),
    ("quark", "Cap diff size served to the UI", "claude"),
    ("firstmate", "Retry remote handoff with jitter", "codex"),
    ("firstmate", "Document watched-tool update check", "claude"),
    ("firstmate", "Speed up session-start digest", "pi"),
    ("website", "Dark mode for docs pages", "claude"),
    (
        "website",
        "Fix broken anchor links in changelog",
        "opencode",
    ),
    ("website", "Compress hero video", "codex"),
    ("quark", "Bump tokio to latest minor", "claude"),
];

/// Pool for background decisions: (question, context, options, recommended).
pub type DecisionTemplate = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    usize,
);

pub const DECISION_TEMPLATES: [DecisionTemplate; 4] = [
    (
        "The migration touches 41 files. Split it into smaller PRs?",
        "The change is mechanical but large. Reviewers have asked for PRs under 400 lines.",
        &[
            (
                "Split into 3 PRs",
                "Easier review; ~1 extra day of rebasing",
            ),
            ("Single PR", "One review; harder to bisect later"),
        ],
        0,
    ),
    (
        "A dependency upgrade changes the public API. How should we handle it?",
        "`axum` 0.8 renamed path captures from `:id` to `{id}`. 12 routes are affected.",
        &[
            ("Upgrade now", "All routes updated in this PR"),
            (
                "Pin old version",
                "No churn; security fixes stop in 6 months",
            ),
            (
                "Upgrade behind a flag",
                "Both syntaxes supported for one release",
            ),
        ],
        0,
    ),
    (
        "Tests are flaky on the macOS runner. What next?",
        "2 of the last 15 runs timed out waiting for the tmux server socket.",
        &[
            ("Increase timeout", "Probably green; root cause stays"),
            ("Investigate", "A scout spends ~1 hour on it"),
            ("Skip on macOS", "CI green; macOS coverage drops"),
            ("Ignore for now", "Red runs keep appearing"),
        ],
        1,
    ),
    (
        "The worker wants to delete 3 unused public functions. Approve?",
        "No callers found in this repo, but they are exported from the crate root.",
        &[
            ("Delete them", "Breaking change for any external callers"),
            ("Deprecate first", "Removed next minor release"),
        ],
        1,
    ),
];
