//! Journey J8: a finished task's learnings arrive as memory proposals; an
//! accepted one lands in the Project repo's `memory/` in its own commit and
//! reaches the coordinator. Any entry can be promoted to user-level memory.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use quark_systems::{EventType, TaskKind, TaskState};
use quarkd::api::{self, AppState};
use quarkd::chat::RecordingInput;
use quarkd::engine::{EngineTask, FleetSnapshot, StatusEntry, StubEngine};
use quarkd::harness::{HarnessRegistry, HostEnv};
use quarkd::projector::Projector;
use quarkd::provision::Layout;
use quarkd::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Harness {
    home: tempfile::TempDir,
    app: Router,
    store: Arc<Store>,
    engine: Arc<StubEngine>,
    chat: Arc<RecordingInput>,
    projector: Projector,
}

fn harness() -> Harness {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let chat = Arc::new(RecordingInput::new());
    let home = tempfile::tempdir().unwrap();
    let harnesses = Arc::new(HarnessRegistry::new(
        quarkd::harness::builtin(),
        HostEnv::default(),
    ));
    let app = api::router(AppState {
        store: store.clone(),
        engine: engine.clone(),
        harnesses: harnesses.clone(),
        accounts: Arc::new(quarkd::accounts::Accounts::new(
            store.clone(),
            harnesses,
            Arc::new(quarkd::accounts::StubQuota::new()),
            &["CLAUDE_CONFIG_DIR"],
        )),
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
        // User-level memory goes where the daemon is told, never `~/.quark`.
        layout: Layout::new(home.path()).with_user_memory(Some(home.path().join("shared"))),
        chat: chat.clone(),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events: quark_eventlog::SqliteEventLog::open(":memory:").unwrap(),
    });
    Harness {
        home,
        app,
        projector: Projector::new(store.clone(), engine.clone())
            .with_session_roots(Default::default()),
        store,
        engine,
        chat,
    }
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .trim_end()
        .to_string()
}

async fn ready_project(h: &Harness) -> String {
    let (status, created) = call(
        &h.app,
        "POST",
        "/v1/projects",
        Some(json!({
            "name": "Quark",
            "repos": [{"url": "https://github.com/quark-systems/quark.git"}],
            "agent_config": {"harness": "claude-code"}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    for _ in 0..200 {
        let (_, p) = call(&h.app, "GET", &format!("/v1/projects/{id}"), None).await;
        if p["status"] == "ready" {
            return id;
        }
        assert_ne!(p["status"], "failed", "{p}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("Project {id} still provisioning");
}

fn task(state: TaskState) -> FleetSnapshot {
    FleetSnapshot {
        tasks: vec![EngineTask {
            id: "fix-42".into(),
            title: "Fix #42".into(),
            kind: Some(TaskKind::Ship),
            state,
            state_note: None,
            harness: Some("claude".into()),
            pull_request_url: Some("https://github.com/quark-systems/quark/pull/42".into()),
            worktree: None,
            terminal: None,
        }],
    }
}

fn line(raw: &str) -> StatusEntry {
    let (head, note) = raw.split_once(':').unwrap();
    StatusEntry {
        kind: head.split('[').next().unwrap().trim().into(),
        decision_key: None,
        note: note.trim().into(),
        raw: raw.into(),
    }
}

fn memory_events(store: &Store) -> Vec<(EventType, Value)> {
    store
        .events_after(0, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type.as_str().starts_with("memory."))
        .map(|e| (e.event_type, e.payload))
        .collect()
}

#[tokio::test]
async fn learnings_become_reviewed_memory() {
    let h = harness();
    let pid = ready_project(&h).await;
    let bare = h.home.path().join("projects").join(format!("{pid}.git"));
    let checkout = h.home.path().join("workspaces").join(&pid).join("project");

    h.engine.set_snapshot(task(TaskState::Running));
    for raw in [
        "working: on it",
        "learned [files=crates/quarkd/src/gates.rs]: Run the gates on the committed head.",
        "learned [source=coordinator]: Keep holdout tests out of worker briefs.",
    ] {
        h.engine.push_status("fix-42", line(raw));
    }
    h.projector.refresh_all().await.unwrap();
    let uri = format!("/v1/projects/{pid}/memory/proposals");
    let (status, list) = call(&h.app, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list, json!([]), "nothing is proposed while the task runs");

    // The worker finished: each learning is proposed once, with evidence.
    h.engine.set_snapshot(task(TaskState::InReview));
    h.projector.refresh_all().await.unwrap();
    h.projector.refresh_all().await.unwrap();
    let (_, list) = call(&h.app, "GET", &uri, None).await;
    let list = list.as_array().unwrap().clone();
    assert_eq!(list.len(), 2, "{list:?}");
    let (a, b) = (&list[0], &list[1]);
    assert_eq!(a["state"], "proposed");
    assert_eq!(a["source"], "worker");
    assert_eq!(a["text"], "Run the gates on the committed head.");
    assert_eq!(a["evidence"]["task_title"], "Fix #42");
    assert!(a["evidence"]["task_id"]
        .as_str()
        .unwrap()
        .starts_with("tsk_"));
    assert_eq!(
        a["evidence"]["pull_request_url"],
        "https://github.com/quark-systems/quark/pull/42"
    );
    assert_eq!(
        a["evidence"]["files"],
        json!(["crates/quarkd/src/gates.rs"])
    );
    assert!(a["proposed_at"].as_str().is_some());
    assert_eq!(b["source"], "coordinator");
    let proposed: Vec<_> = memory_events(&h.store)
        .into_iter()
        .map(|(t, p)| (t, p["id"].clone()))
        .collect();
    assert_eq!(
        proposed,
        [
            (EventType::MemoryProposed, a["id"].clone()),
            (EventType::MemoryProposed, b["id"].clone())
        ]
    );

    // Accept the first, edited: one new file in its own commit on main.
    let before = git(&bare, &["rev-parse", "main"]);
    let a_id = a["id"].as_str().unwrap();
    let (status, accepted) = call(
        &h.app,
        "POST",
        &format!("{uri}/{a_id}:accept"),
        Some(json!({"text": "Run the gates on the committed head, never the working tree.", "decided_by": "matt"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["state"], "accepted");
    assert_eq!(accepted["decided_by"], "matt");
    assert_eq!(
        accepted["text"],
        "Run the gates on the committed head, never the working tree."
    );
    let entry = &accepted["entry"];
    let path = entry["path"].as_str().unwrap();
    let day = &a["proposed_at"].as_str().unwrap()[..10];
    assert_eq!(
        path,
        format!("memory/{day}-run-the-gates-on-the-committed-head-never-the.md")
    );
    let commit = entry["commit"].as_str().unwrap();
    assert_eq!(git(&bare, &["rev-parse", "main"]), commit);
    assert_eq!(git(&bare, &["rev-parse", "main^"]), before);
    assert_eq!(
        git(&bare, &["show", "--name-only", "--format=%s", "main"]),
        format!("Remember: Run the gates on the committed head, never the working tree.\n\n{path}")
    );
    let file = git(&bare, &["show", &format!("main:{path}")]);
    assert!(file.contains("task_title: \"Fix #42\""), "{file}");
    assert!(
        file.contains("files: [\"crates/quarkd/src/gates.rs\"]"),
        "{file}"
    );
    assert!(file.ends_with("\n---\n\nRun the gates on the committed head, never the working tree."));
    assert!(
        checkout.join(path).is_file(),
        "the coordinator's checkout has it"
    );

    // The coordinator is told, so it reads the entry on its next turn.
    let mut told = Vec::new();
    for _ in 0..100 {
        told = h.chat.sent();
        if !told.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(told.len(), 1);
    assert_eq!(told[0].0.project_id, pid);
    assert!(
        told[0].1.contains(&format!("project/{path}")),
        "{}",
        told[0].1
    );
    assert!(told[0].1.ends_with("never the working tree."));

    // Memory lists the committed entry.
    let (status, entries) = call(&h.app, "GET", &format!("/v1/projects/{pid}/memory"), None).await;
    assert_eq!(status, StatusCode::OK);
    let entries = entries.as_array().unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["path"], path);
    assert_eq!(entries[0]["text"], accepted["text"]);
    assert_eq!(entries[0]["evidence"], a["evidence"]);
    assert_eq!(entries[0]["date"], a["proposed_at"]);
    assert_eq!(entries[0]["proposal_id"], a["id"]);
    assert_eq!(entries[0]["accepted_by"], "matt");

    // Decisions are final; the second proposal is rejected without a commit.
    let (status, err) = call(&h.app, "POST", &format!("{uri}/{a_id}:accept"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(err["error"]["code"], "already_decided");
    let b_id = b["id"].as_str().unwrap();
    let (status, rejected) = call(&h.app, "POST", &format!("{uri}/{b_id}:reject"), None).await;
    assert_eq!(status, StatusCode::OK, "{rejected}");
    assert_eq!(rejected["state"], "rejected");
    assert!(rejected["decided_by"].as_str().is_some());
    let (status, _) = call(&h.app, "POST", &format!("{uri}/{b_id}:accept"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(git(&bare, &["rev-parse", "main"]), commit);

    let (_, proposed) = call(&h.app, "GET", &format!("{uri}?state=proposed"), None).await;
    assert_eq!(proposed, json!([]));
    let types: Vec<_> = memory_events(&h.store)
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    assert_eq!(
        types,
        [
            EventType::MemoryProposed,
            EventType::MemoryProposed,
            EventType::MemoryAccepted,
            EventType::MemoryRejected
        ]
    );
    let (_, last) = memory_events(&h.store).pop().unwrap();
    assert_eq!(last["id"], b["id"]);
}

#[tokio::test]
async fn refuses_what_it_cannot_do() {
    let h = harness();
    let (_, attached) = call(
        &h.app,
        "POST",
        "/v1/projects",
        Some(json!({"name": "Attached", "workspace_path": "/nowhere"})),
    )
    .await;
    let pid = attached["id"].as_str().unwrap();

    let (status, _) = call(&h.app, "GET", "/v1/projects/nope/memory/proposals", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&h.app, "GET", "/v1/projects/nope/memory", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A Project without a Project repo has no memory yet.
    let (status, entries) = call(&h.app, "GET", &format!("/v1/projects/{pid}/memory"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(entries, json!([]));

    h.engine.set_snapshot(task(TaskState::Done));
    h.engine
        .push_status("fix-42", line("learned: Attached Projects learn too."));
    h.projector.refresh_all().await.unwrap();
    let uri = format!("/v1/projects/{pid}/memory/proposals");
    let (_, list) = call(&h.app, "GET", &uri, None).await;
    let id = list[0]["id"].as_str().unwrap().to_string();

    let (status, err) = call(&h.app, "POST", &format!("{uri}/{id}:accept"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], "no_project_repo");
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("{uri}/{id}:accept"),
        Some(json!({"text": "  "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&h.app, "POST", &format!("{uri}/{id}:archive"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&h.app, "POST", &format!("{uri}/{id}"), None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, _) = call(&h.app, "POST", &format!("{uri}/mpr_nope:reject"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/other/memory/proposals/{id}:reject"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(h.chat.sent().is_empty());
}

/// Finishes a task that learned `text` and accepts the proposal.
async fn accepted(h: &Harness, pid: &str, text: &str) -> Value {
    h.engine.set_snapshot(task(TaskState::Done));
    h.engine
        .push_status("fix-42", line(&format!("learned: {text}")));
    h.projector.refresh_all().await.unwrap();
    let uri = format!("/v1/projects/{pid}/memory/proposals");
    let (_, list) = call(&h.app, "GET", &format!("{uri}?state=proposed"), None).await;
    let id = list[0]["id"].as_str().unwrap().to_string();
    let (status, accepted) = call(&h.app, "POST", &format!("{uri}/{id}:accept"), None).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    accepted
}

#[tokio::test]
async fn entries_link_their_commit_and_promote_to_user_memory() {
    let h = harness();
    let pid = ready_project(&h).await;
    let bare = h.home.path().join("projects").join(format!("{pid}.git"));
    let shared = h.home.path().join("shared");
    let entry = accepted(&h, &pid, "Rebase onto main before pushing.").await["entry"].clone();
    let (id, commit) = (
        entry["id"].as_str().unwrap(),
        entry["commit"].as_str().unwrap(),
    );
    let memory = format!("/v1/projects/{pid}/memory");

    // A listed entry names the commit that added it, and the commit is served.
    let (_, entries) = call(&h.app, "GET", &memory, None).await;
    assert_eq!(entries[0]["commit"], commit, "{entries}");
    let (status, shown) = call(&h.app, "GET", &format!("{memory}/commits/{commit}"), None).await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(shown["commit"], commit);
    assert_eq!(
        shown["subject"],
        "Remember: Rebase onto main before pushing."
    );
    assert!(shown["date"].as_str().is_some());
    let patch = shown["patch"].as_str().unwrap();
    assert!(
        patch.starts_with(&format!("diff --git a/memory/{id}.md b/memory/{id}.md")),
        "{patch}"
    );
    assert!(
        patch.ends_with("+Rebase onto main before pushing."),
        "{patch}"
    );
    // The commit that created the repo is on main too, without the entry.
    let root = git(&bare, &["rev-list", "--max-parents=0", "main"]);
    let (status, first) = call(&h.app, "GET", &format!("{memory}/commits/{root}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!first["patch"].as_str().unwrap().contains("Rebase"));
    for bad in ["0000000000000000000000000000000000000000", "main", "abc"] {
        let (status, _) = call(&h.app, "GET", &format!("{memory}/commits/{bad}"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{bad}");
    }

    // Nothing is shared until an entry is promoted.
    let (status, none) = call(&h.app, "GET", "/v1/memory", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(none, json!([]));
    assert!(!shared.exists());
    for _ in 0..100 {
        if !h.chat.sent().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let (status, promoted) = call(
        &h.app,
        "POST",
        &format!("{memory}/{id}:promote"),
        Some(json!({"promoted_by": "matt"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{promoted}");
    let file = shared.join(format!("{id}.md"));
    assert_eq!(promoted["id"], id);
    assert_eq!(promoted["path"], file.to_str().unwrap());
    assert_eq!(promoted["text"], "Rebase onto main before pushing.");
    assert_eq!(promoted["project_id"], pid.as_str());
    assert_eq!(promoted["project_name"], "Quark");
    assert_eq!(promoted["entry_id"], id);
    assert_eq!(promoted["commit"], commit);
    assert_eq!(promoted["promoted_by"], "matt");
    assert_eq!(promoted["evidence"], entry["evidence"]);
    let body = std::fs::read_to_string(&file).unwrap();
    assert!(body.contains("task_title: \"Fix #42\""), "{body}");
    assert!(body.contains(&format!("project: \"{pid}\"")), "{body}");
    assert!(body.ends_with("---\n\nRebase onto main before pushing.\n"));
    assert!(
        !h.home.path().join("memory").exists(),
        "only the configured directory is written"
    );
    // The Project keeps its entry, and its repo is untouched.
    assert_eq!(git(&bare, &["rev-parse", "main"]), commit);

    // Every coordinator is told once; promoting again changes nothing.
    let mut told = Vec::new();
    for _ in 0..100 {
        told = h.chat.sent();
        if told.len() > 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(told.len(), 2, "{told:?}");
    assert!(told[1].1.contains(file.to_str().unwrap()), "{}", told[1].1);
    let (status, again) = call(&h.app, "POST", &format!("{memory}/{id}:promote"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again, promoted);
    let (_, listed) = call(&h.app, "GET", "/v1/memory", None).await;
    assert_eq!(listed, json!([promoted]));
    assert_eq!(std::fs::read_dir(&shared).unwrap().count(), 1);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.chat.sent().len(), 2);

    let (status, _) = call(&h.app, "POST", &format!("{memory}/nope:promote"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&h.app, "POST", &format!("{memory}/{id}:demote"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&h.app, "POST", &format!("{memory}/{id}"), None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/nope/memory/{id}:promote"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("{memory}/{id}:promote"),
        Some(json!({"promoted_by": "a\nb"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
