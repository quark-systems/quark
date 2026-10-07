//! Board and task detail: task activity from status logs, live refresh on
//! file changes, and changed files and diffs of a task's working tree.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use quark_systems::{AgentConfig, CreateProject, DeliveryPolicy, EventType, TaskKind, TaskState};
use quarkd::api::{self, AppState};
use quarkd::chat::RecordingInput;
use quarkd::engine::{
    EngineAdapter, EngineError, EngineTask, FleetSnapshot, Hold, SourceRepo, StatusEntry,
    StatusTail, StubEngine, TaskControl, WorkspacePlan, WorkspaceRef,
};
use quarkd::harness::{HarnessRegistry, HostEnv};
use quarkd::projector::Projector;
use quarkd::provision::Layout;
use quarkd::store::Store;
use serde_json::Value;
use tower::ServiceExt;

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn engine_task(id: &str, worktree: Option<PathBuf>) -> EngineTask {
    EngineTask {
        id: id.into(),
        title: format!("Task {id}"),
        kind: Some(TaskKind::Ship),
        state: TaskState::Running,
        state_note: None,
        state_source: None,
        harness: Some("claude".into()),
        pull_request_url: None,
        terminal: None,
        worktree,
    }
}

fn entry(kind: &str, key: Option<&str>, note: &str) -> StatusEntry {
    StatusEntry {
        kind: kind.into(),
        decision_key: key.map(str::to_string),
        note: note.into(),
        raw: format!("{kind}: {note}"),
    }
}

struct Setup {
    _home: tempfile::TempDir,
    app: Router,
    store: Arc<Store>,
    engine: Arc<StubEngine>,
    projector: Projector,
    project_id: String,
}

fn setup(workspace: &Path) -> Setup {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let project_id = store
        .create_project(CreateProject {
            name: "Quark".into(),
            goal: None,
            workspace_path: Some(workspace.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .unwrap()
        .id;
    let home = tempfile::tempdir().unwrap();
    let harnesses = Arc::new(HarnessRegistry::new(
        quarkd::harness::builtin(),
        HostEnv::default(),
    ));
    Setup {
        app: api::router(AppState {
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
            layout: Layout::new(home.path()),
            chat: Arc::new(RecordingInput::new()),
            forge: Arc::new(quarkd::forge::StubForge::new()),
            events: quark_eventlog::SqliteEventLog::open(":memory:").unwrap(),
            triggers: None,
        }),
        _home: home,
        projector: Projector::new(store.clone(), engine.clone())
            .with_session_roots(Default::default()),
        store,
        engine,
        project_id,
    }
}

#[tokio::test]
async fn status_entries_become_task_events() {
    let s = setup(Path::new("/tmp/ws"));
    s.engine.set_snapshot(FleetSnapshot {
        tasks: vec![engine_task("fix-login", None)],
    });
    s.engine
        .push_status("fix-login", entry("working", None, "reading the code"));
    s.projector.refresh_all().await.unwrap();
    s.engine.push_status(
        "fix-login",
        entry("needs-decision", Some("flag"), "ship behind a flag?"),
    );
    s.projector.refresh_all().await.unwrap();
    // Nothing new: no duplicate entries.
    s.projector.refresh_all().await.unwrap();

    let (_, tasks) = get(&s.app, &format!("/v1/projects/{}/tasks", s.project_id)).await;
    let tid = tasks[0]["id"].as_str().unwrap();

    let (status, log) = get(&s.app, &format!("/v1/tasks/{tid}/events")).await;
    assert_eq!(status, StatusCode::OK);
    let log = log.as_array().unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0]["kind"], "working");
    assert_eq!(log[1]["kind"], "needs-decision");
    assert_eq!(log[1]["decision_key"], "flag");
    assert_eq!(log[1]["note"], "ship behind a flag?");
    assert_eq!(log[1]["task_id"], tid);

    let after = log[0]["id"].as_i64().unwrap();
    let (_, page) = get(&s.app, &format!("/v1/tasks/{tid}/events?after={after}")).await;
    assert_eq!(page.as_array().unwrap().len(), 1);

    let types: Vec<_> = s
        .store
        .events_after(0, 100)
        .unwrap()
        .into_iter()
        .map(|e| e.event_type)
        .collect();
    assert_eq!(
        types,
        [
            EventType::ProjectUpdated,
            EventType::TaskCreated,
            EventType::TaskEvent,
            EventType::TaskEvent
        ]
    );

    let (status, _) = get(&s.app, "/v1/tasks/nope/events").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The stub engine, but watching a real directory for `.status` files.
struct Watched {
    inner: Arc<StubEngine>,
    dir: PathBuf,
}

#[async_trait]
impl EngineAdapter for Watched {
    fn name(&self) -> &'static str {
        "watched"
    }
    async fn snapshot(&self, ws: &WorkspaceRef) -> Result<FleetSnapshot, EngineError> {
        self.inner.snapshot(ws).await
    }
    async fn status_tail(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        offset: u64,
    ) -> Result<StatusTail, EngineError> {
        self.inner.status_tail(ws, task_id, offset).await
    }
    async fn holds(&self, ws: &WorkspaceRef) -> Result<Vec<Hold>, EngineError> {
        self.inner.holds(ws).await
    }
    async fn send_message(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        text: &str,
    ) -> Result<(), EngineError> {
        self.inner.send_message(ws, task_id, text).await
    }
    async fn answer(
        &self,
        ws: &WorkspaceRef,
        hold_id: &str,
        answer: &str,
        answered_by: &str,
    ) -> Result<(), EngineError> {
        self.inner.answer(ws, hold_id, answer, answered_by).await
    }
    async fn control(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        action: &TaskControl,
    ) -> Result<(), EngineError> {
        self.inner.control(ws, task_id, action).await
    }
    async fn merge_pull_request(
        &self,
        ws: &WorkspaceRef,
        task_id: &str,
        url: &str,
        method: Option<quark_systems::MergeMethod>,
    ) -> Result<(), EngineError> {
        self.inner
            .merge_pull_request(ws, task_id, url, method)
            .await
    }
    async fn set_standing_approval(
        &self,
        ws: &WorkspaceRef,
        repos: &[String],
        on: bool,
    ) -> Result<(), EngineError> {
        self.inner.set_standing_approval(ws, repos, on).await
    }
    async fn add_source(
        &self,
        command: &Path,
        source: &SourceRepo,
        delivery: DeliveryPolicy,
    ) -> Result<(), EngineError> {
        self.inner.add_source(command, source, delivery).await
    }
    async fn seed_workspace(
        &self,
        command: &Path,
        plan: &WorkspacePlan,
    ) -> Result<PathBuf, EngineError> {
        self.inner.seed_workspace(command, plan).await
    }
    async fn start_coordinator(
        &self,
        command: &Path,
        ws: &WorkspaceRef,
        agent: &AgentConfig,
        account_env: &[(String, String)],
        resume: bool,
    ) -> Result<(), EngineError> {
        self.inner
            .start_coordinator(command, ws, agent, account_env, resume)
            .await
    }
    fn watch_dirs(&self, _ws: &WorkspaceRef) -> Vec<PathBuf> {
        vec![self.dir.clone()]
    }
    fn is_task_change(&self, path: &Path) -> bool {
        path.extension().is_some_and(|e| e == "status")
    }
}

async fn wait_for(
    rx: &mut tokio::sync::broadcast::Receiver<quark_systems::Event>,
    want: EventType,
) -> quark_systems::Event {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let e = rx.recv().await.unwrap();
            if e.event_type == want {
                return e;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {want:?} within 10s"))
}

#[tokio::test]
async fn a_status_write_refreshes_the_board_before_the_timer() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let stub = Arc::new(StubEngine::new());
    let engine = Arc::new(Watched {
        inner: stub.clone(),
        dir: dir.path().to_path_buf(),
    });
    store
        .create_project(CreateProject {
            name: "Quark".into(),
            goal: None,
            workspace_path: Some(dir.path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .unwrap();
    let mut live = store.subscribe();
    // The timer fires once at start, then not again during the test.
    let projector =
        tokio::spawn(Projector::new(store.clone(), engine).run(Duration::from_secs(3600)));

    // Let the start-up refresh finish and the watcher attach.
    tokio::time::sleep(Duration::from_millis(500)).await;
    stub.set_snapshot(FleetSnapshot {
        tasks: vec![engine_task("t1", None)],
    });
    stub.push_status("t1", entry("working", None, "started"));
    std::fs::write(dir.path().join("t1.status"), "working: started\n").unwrap();
    wait_for(&mut live, EventType::TaskCreated).await;
    let e = wait_for(&mut live, EventType::TaskEvent).await;
    assert_eq!(e.payload["note"], "started");

    // Engine bookkeeping files do not trigger a refresh.
    let mut changed = engine_task("t1", None);
    changed.state = TaskState::Done;
    stub.set_snapshot(FleetSnapshot {
        tasks: vec![changed],
    });
    std::fs::write(dir.path().join(".last-beat"), "x").unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        live.try_recv().is_err(),
        "bookkeeping writes must not refresh"
    );
    std::fs::write(dir.path().join("t1.status"), "working: started\ndone: x\n").unwrap();
    let e = wait_for(&mut live, EventType::TaskStateChanged).await;
    assert_eq!(e.payload["task"]["state"], "done");
    projector.abort();
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// A repo on `main` with a task branch that commits, renames, edits without
/// committing and adds an untracked file.
fn task_repo(dir: &Path) -> PathBuf {
    let wt = dir.join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    git(&wt, &["init", "-q", "-b", "main"]);
    std::fs::write(wt.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(wt.join("old.rs"), "fn main() {}\n".repeat(10)).unwrap();
    std::fs::write(wt.join("gone.txt"), "bye\n").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-q", "-m", "base"]);
    git(&wt, &["checkout", "-q", "-b", "fm/t1"]);
    git(&wt, &["mv", "old.rs", "new.rs"]);
    git(&wt, &["rm", "-q", "gone.txt"]);
    git(&wt, &["commit", "-q", "-m", "task work"]);
    // main moves on; the task is still compared with where it branched.
    git(&wt, &["checkout", "-q", "main"]);
    std::fs::write(wt.join("later.txt"), "later\n").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-q", "-m", "later on main"]);
    git(&wt, &["checkout", "-q", "fm/t1"]);
    std::fs::write(wt.join("a.txt"), "one\n2\nthree\n").unwrap();
    std::fs::write(wt.join("notes new.md"), "# hi\nthere").unwrap();
    wt
}

#[tokio::test]
async fn changes_and_diff_cover_committed_uncommitted_and_untracked_work() {
    let dir = tempfile::tempdir().unwrap();
    let wt = task_repo(dir.path());
    let s = setup(dir.path());
    s.engine.set_snapshot(FleetSnapshot {
        tasks: vec![
            engine_task("t1", Some(wt.clone())),
            engine_task("queued", None),
        ],
    });
    s.projector.refresh_all().await.unwrap();
    let (_, tasks) = get(&s.app, &format!("/v1/projects/{}/tasks", s.project_id)).await;
    let tid = tasks[0]["id"].as_str().unwrap();
    let queued = tasks[1]["id"].as_str().unwrap();

    let (status, c) = get(&s.app, &format!("/v1/tasks/{tid}/changes")).await;
    assert_eq!(status, StatusCode::OK, "{c}");
    assert_eq!(c["base_ref"], "main");
    assert_eq!(c["head"].as_str().unwrap().len(), 40);
    let files: Vec<_> = c["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_string(),
                f["status"].as_str().unwrap().to_string(),
                f["additions"].as_u64(),
                f["deletions"].as_u64(),
            )
        })
        .collect();
    let want =
        |p: &str, st: &str, a: u64, d: u64| (p.to_string(), st.to_string(), Some(a), Some(d));
    assert_eq!(
        files,
        [
            want("a.txt", "modified", 2, 1),
            want("gone.txt", "deleted", 0, 1),
            want("new.rs", "renamed", 0, 0),
            want("notes new.md", "untracked", 2, 0),
        ],
        "later.txt landed on main after the branch point and is not the task's"
    );
    assert_eq!(c["files"][2]["old_path"], "old.rs");

    let (status, d) = get(&s.app, &format!("/v1/tasks/{tid}/diff")).await;
    assert_eq!(status, StatusCode::OK);
    let patch = d["patch"].as_str().unwrap();
    for needle in [
        "+three",
        "rename to new.rs",
        "deleted file mode",
        "+++ b/notes new.md",
    ] {
        assert!(patch.contains(needle), "missing {needle:?} in\n{patch}");
    }
    assert_eq!(d["truncated"], false);
    assert_eq!(d["base"], c["base"]);

    let (status, d) = get(&s.app, &format!("/v1/tasks/{tid}/diff?path=a.txt")).await;
    assert_eq!(status, StatusCode::OK);
    let patch = d["patch"].as_str().unwrap();
    assert!(
        patch.contains("+three") && !patch.contains("new.rs"),
        "{patch}"
    );
    assert_eq!(d["path"], "a.txt");

    let (status, d) = get(&s.app, &format!("/v1/tasks/{tid}/diff?path=notes%20new.md")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(d["patch"].as_str().unwrap().contains("+there"));

    // Only changed files can be read, so the endpoint cannot read anything else.
    for p in ["later.txt", "../etc/passwd", "*.txt"] {
        let (status, body) = get(&s.app, &format!("/v1/tasks/{tid}/diff?path={p}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{p}: {body}");
    }

    let (status, body) = get(&s.app, &format!("/v1/tasks/{queued}/changes")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "no_worktree");

    std::fs::remove_dir_all(&wt).unwrap();
    let (status, body) = get(&s.app, &format!("/v1/tasks/{tid}/changes")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "worktree_unavailable");
}

#[tokio::test]
async fn reading_changes_leaves_the_worktree_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let wt = task_repo(dir.path());
    let index = wt.join(".git/index");
    let before = std::fs::read(&index).unwrap();
    let before_mtime = std::fs::metadata(&index).unwrap().modified().unwrap();
    // Make the index stat-dirty so a locking git status would rewrite it.
    std::fs::write(wt.join("new.rs"), "fn main() {}\n".repeat(10)).unwrap();
    quarkd::worktree::changes(&wt).await.unwrap();
    quarkd::worktree::diff(&wt, None).await.unwrap();
    assert_eq!(std::fs::read(&index).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&index).unwrap().modified().unwrap(),
        before_mtime
    );
    assert!(!wt.join(".git/index.lock").exists());
}
