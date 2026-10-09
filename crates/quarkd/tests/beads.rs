//! Beads as the Project's issues and memory: setting up the Project's
//! database, the New issue side chat (the coordinator drafts, the user
//! accepts, the beads appear with their links), accepting a learning
//! into the Project's Beads memories, and decisions as `decision` beads
//! with gates, answered on either side.
//!
//! The journey drives the real `bd` (and `dolt`, which server mode runs).
//! Where they are not installed it checks only what needs neither and says
//! so; set `QUARK_BD` to use a `bd` that is not on `PATH`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use quark_systems::{TaskKind, TaskState};
use quarkd::api::{self, AppState};
use quarkd::beads::Beads;
use quarkd::chat::RecordingInput;
use quarkd::engine::{EngineTask, FleetSnapshot, Hold, StatusEntry, StubEngine, StubWrite};
use quarkd::harness::{HarnessRegistry, HostEnv};
use quarkd::projector::Projector;
use quarkd::provision::Layout;
use quarkd::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Harness {
    home: tempfile::TempDir,
    app: Router,
    state: AppState,
    store: Arc<Store>,
    engine: Arc<StubEngine>,
    chat: Arc<RecordingInput>,
    projector: Projector,
}

impl Drop for Harness {
    /// Server mode leaves a Dolt sql-server running for the database.
    fn drop(&mut self) {
        let beads = self.home.path().join("beads");
        let Ok(dirs) = std::fs::read_dir(beads) else {
            return;
        };
        for dir in dirs.flatten() {
            let _ = std::process::Command::new(bd())
                .args(["dolt", "stop"])
                .current_dir(dir.path())
                .output();
        }
    }
}

fn bd() -> String {
    std::env::var("QUARK_BD").unwrap_or_else(|_| "bd".into())
}

/// Whether `bd` and `dolt` both run here.
fn installed() -> bool {
    let runs = |bin: &str| {
        std::process::Command::new(bin)
            .arg("version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    runs(&bd()) && runs("dolt")
}

fn harness() -> Harness {
    harness_with(true)
}

/// With `auto_setup` off, a new Project has no database until `:setup`.
fn harness_with(auto_setup: bool) -> Harness {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let chat = Arc::new(RecordingInput::new());
    let home = tempfile::tempdir().unwrap();
    let harnesses = Arc::new(HarnessRegistry::new(
        quarkd::harness::builtin(),
        HostEnv::default(),
    ));
    let state = AppState {
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
        layout: Layout::new(home.path()).with_user_memory(Some(home.path().join("shared"))),
        chat: chat.clone(),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events: quark_eventlog::SqliteEventLog::open(":memory:").unwrap(),
        triggers: None,
        beads: Arc::new({
            let b = Beads::new(Some(bd().into()), "http://127.0.0.1:7380".into());
            if auto_setup {
                b.with_auto_setup()
            } else {
                b
            }
        }),
    };
    Harness {
        home,
        app: api::router(state.clone()),
        state,
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

/// Waits for the database the Project got when it was created.
async fn set_up(h: &Harness, pid: &str) -> Value {
    for _ in 0..600 {
        let (_, s) = call(&h.app, "GET", &format!("/v1/projects/{pid}/beads"), None).await;
        match s["state"].as_str() {
            Some("ready") => return s,
            Some("setting_up") => tokio::time::sleep(Duration::from_millis(100)).await,
            _ => panic!("setup did not finish: {s}"),
        }
    }
    panic!("Beads still setting up");
}

#[tokio::test]
async fn drafts_need_a_project_and_a_database() {
    let h = harness_with(false);
    let (status, _) = call(&h.app, "GET", "/v1/projects/nope/beads", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let pid = ready_project(&h).await;

    let (_, s) = call(&h.app, "GET", &format!("/v1/projects/{pid}/beads"), None).await;
    let expected = if installed() {
        "missing"
    } else {
        "unavailable"
    };
    assert_eq!(s["state"], expected, "{s}");
    let (status, e) = call(&h.app, "GET", &format!("/v1/projects/{pid}/issues"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["error"]["code"], "no_beads");

    // A draft does not need the database until it is accepted.
    let uri = format!("/v1/projects/{pid}/issue-drafts");
    let (status, _) = call(&h.app, "POST", &uri, Some(json!({"text": "  "}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, d) = call(
        &h.app,
        "POST",
        &uri,
        Some(json!({"text": "PRs that go red"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{d}");
    let did = d["id"].as_str().unwrap();
    let (status, bad) = call(
        &h.app,
        "PUT",
        &format!("/v1/issue-drafts/{did}"),
        Some(json!({"issues": [{"key": "1", "title": ""}]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    let (status, e) = call(
        &h.app,
        "POST",
        &format!("/v1/issue-drafts/{did}:accept"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{e}");
    let (status, gone) = call(
        &h.app,
        "POST",
        &format!("/v1/issue-drafts/{did}:discard"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(gone["state"], "discarded");
    let (_, open) = call(&h.app, "GET", &uri, None).await;
    assert_eq!(open, json!([]));

    // Without a coordinator the draft stops waiting and says why.
    h.chat.set_outcome(Err("no session".into()));
    let (_, d) = call(&h.app, "POST", &uri, Some(json!({"text": "again"}))).await;
    assert_eq!(d["waiting"], false);
    assert_eq!(d["messages"][1]["role"], "coordinator");
    assert!(d["messages"][1]["text"]
        .as_str()
        .unwrap()
        .contains("no session"));
}

#[tokio::test]
async fn the_coordinator_drafts_and_accepting_creates_the_beads() {
    if !installed() {
        eprintln!("skipped: bd and dolt are not both installed");
        return;
    }
    let h = harness();
    let pid = ready_project(&h).await;
    let s = set_up(&h, &pid).await;
    assert_eq!(s["prefix"], "quark");
    let dir = h.home.path().join("beads").join(&pid).join(".beads");
    assert_eq!(s["dir"], dir.parent().unwrap().display().to_string());
    // The coordinator, and through it every worker, finds the database
    // through BEADS_DIR rather than a `.beads` in some repo.
    let env = h
        .engine
        .writes()
        .into_iter()
        .find_map(|w| match w {
            StubWrite::StartCoordinator { account_env, .. } => Some(account_env),
            _ => None,
        })
        .unwrap();
    assert!(
        env.contains(&("BEADS_DIR".into(), dir.display().to_string())),
        "{env:?}"
    );

    // Nothing syncs with a tracker until a rule says so.
    assert_eq!(s["sync_rules"], json!([]));
    let sync = format!("/v1/projects/{pid}/beads:sync");
    let (status, e) = call(&h.app, "POST", &sync, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{e}");
    let rules = format!("/v1/projects/{pid}/beads/sync-rules");
    let (status, e) = call(
        &h.app,
        "PUT",
        &rules,
        Some(json!({"rules": [{"repository": "not a repo", "direction": "both"}]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{e}");
    let (status, r) = call(
        &h.app,
        "PUT",
        &rules,
        Some(json!({"rules": [
            {"repository": "quark-systems/quark", "direction": "both"},
            {"repository": "quark-systems/web", "direction": "pull", "enabled": false}
        ]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["sync_rules"][0]["label"], "repo:quark");
    assert_eq!(r["sync_rules"][0]["tracker"], "github");
    assert_eq!(r["sync_rules"][1]["enabled"], false);
    let (_, again) = call(&h.app, "GET", &format!("/v1/projects/{pid}/beads"), None).await;
    assert_eq!(again["sync_rules"], r["sync_rules"]);
    let (status, off) = call(&h.app, "PUT", &rules, Some(json!({"rules": []}))).await;
    assert_eq!(status, StatusCode::OK, "{off}");
    assert_eq!(off["sync_rules"], json!([]));

    let uri = format!("/v1/projects/{pid}/issue-drafts");
    let (status, d) = call(
        &h.app,
        "POST",
        &uri,
        Some(json!({"text": "When a PR goes red after I looked, I don't find out."})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{d}");
    assert_eq!(d["waiting"], true);
    let did = d["id"].as_str().unwrap().to_string();
    let (_, asked) = h.chat.sent().last().cloned().unwrap();
    assert!(asked.contains("When a PR goes red"), "{asked}");
    assert!(
        asked.contains(&format!("/v1/issue-drafts/{did}")),
        "{asked}"
    );

    // The coordinator answers as the ask tells it to.
    let drafts = json!({
        "reply": "Two pieces of work, the second after the first.",
        "issues": [
            {"key": "1", "title": "A PR that goes red asks for attention again", "issue_type": "bug",
             "priority": 1, "labels": ["attention"], "description": "Set the flag again."},
            {"key": "2", "title": "Push a phone notification when a PR goes red", "issue_type": "feature",
             "blocked_by": ["1"]}
        ]
    });
    let (status, d) = call(
        &h.app,
        "PUT",
        &format!("/v1/issue-drafts/{did}"),
        Some(drafts),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{d}");
    assert_eq!(d["waiting"], false);
    assert_eq!(d["issues"][1]["priority"], 2, "defaults fill in");

    let (_, d) = call(
        &h.app,
        "POST",
        &format!("/v1/issue-drafts/{did}/messages"),
        Some(json!({"text": "Make the first one P0"})),
    )
    .await;
    assert_eq!(d["waiting"], true);
    let (_, asked) = h.chat.sent().last().cloned().unwrap();
    assert!(asked.contains("Make the first one P0") && asked.contains("A PR that goes red"));

    // Accepting with an edit creates both, linked.
    let mut edited = d["issues"].clone();
    edited[0]["priority"] = json!(0);
    let (status, done) = call(
        &h.app,
        "POST",
        &format!("/v1/issue-drafts/{did}:accept"),
        Some(json!({"issues": edited, "start_worker": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["state"], "accepted");
    let first = done["created"]["1"].as_str().unwrap().to_string();
    let second = done["created"]["2"].as_str().unwrap().to_string();
    assert!(first.starts_with("quark-"), "{first}");
    let (_, asked) = h.chat.sent().last().cloned().unwrap();
    assert_eq!(
        asked,
        format!("Start a worker on {first}: A PR that goes red asks for attention again")
    );
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/issue-drafts/{did}:accept"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "accepted once");

    let (_, ready) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{pid}/issues?filter=ready"),
        None,
    )
    .await;
    let ready: Vec<&str> = ready
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ready, [first.as_str()]);
    let (_, blocked) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{pid}/issues?filter=blocked"),
        None,
    )
    .await;
    assert_eq!(blocked[0]["id"], second);
    assert_eq!(blocked[0]["blocked_by"], json!([first]));

    let (status, one) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{pid}/issues/{first}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert_eq!(one["priority"], 0);
    assert_eq!(one["labels"], json!(["attention"]));
    assert_eq!(one["blocks_issues"][0]["id"], second);
    let (status, _) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{pid}/issues/quark-nope"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The journal reports changes from anyone, here an agent closing one.
    let changes = |h: &Harness| {
        h.store
            .events_after(0, 10_000)
            .unwrap()
            .into_iter()
            .filter(|e| e.event_type.as_str() == "beads.changed")
            .count()
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    let before = changes(&h);
    let dir = h.home.path().join("beads").join(&pid);
    let out = std::process::Command::new(bd())
        .args(["close", &first])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for _ in 0..100 {
        if changes(&h) > before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(changes(&h) > before, "no beads.changed for the close");
    let (_, ready) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{pid}/issues?filter=ready"),
        None,
    )
    .await;
    assert_eq!(
        ready[0]["id"], second,
        "closing the blocker readies the next"
    );
}

/// The last message the coordinator was sent, which goes out in the
/// background.
async fn told(h: &Harness) -> String {
    for _ in 0..100 {
        if let Some((_, text)) = h.chat.sent().last() {
            return text.clone();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the coordinator was told nothing");
}

fn task(state: TaskState) -> FleetSnapshot {
    FleetSnapshot {
        tasks: vec![EngineTask {
            id: "fix-42".into(),
            title: "Fix #42".into(),
            kind: Some(TaskKind::Ship),
            state,
            state_note: None,
            state_source: None,
            harness: Some("claude".into()),
            pull_request_url: Some("https://github.com/quark-systems/quark/pull/42".into()),
            worktree: None,
            terminal: None,
        }],
    }
}

fn learned(text: &str) -> StatusEntry {
    StatusEntry {
        kind: "learned".into(),
        decision_key: None,
        note: text.into(),
        raw: format!("learned: {text}"),
    }
}

#[tokio::test]
async fn an_accepted_learning_becomes_a_beads_memory_or_user_memory() {
    if !installed() {
        eprintln!("skipped: bd and dolt are not both installed");
        return;
    }
    let h = harness();
    let pid = ready_project(&h).await;
    set_up(&h, &pid).await;
    h.engine.set_snapshot(task(TaskState::Running));
    h.engine.push_status(
        "fix-42",
        learned("tmux misses pane exits; run-shell true reaps them"),
    );
    h.engine
        .push_status("fix-42", learned("Run the app's e2e suite serially on CI"));
    h.projector.refresh_all().await.unwrap();
    h.engine.set_snapshot(task(TaskState::Done));
    h.projector.refresh_all().await.unwrap();
    h.projector.refresh_all().await.unwrap();
    let uri = format!("/v1/projects/{pid}/memory/proposals");
    let (_, list) = call(&h.app, "GET", &format!("{uri}?state=proposed"), None).await;
    assert_eq!(list.as_array().map(Vec::len), Some(2), "{list}");
    let (a, b) = (
        list[0]["id"].as_str().unwrap(),
        list[1]["id"].as_str().unwrap(),
    );

    let (status, accepted) = call(&h.app, "POST", &format!("{uri}/{a}:accept"), None).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    let key = "tmux-misses-pane-exits-run-shell";
    assert_eq!(accepted["entry"]["beads_key"], key);
    assert_eq!(accepted["entry"]["path"], format!("beads:{key}"));
    let told = told(&h).await;
    assert!(told.contains(&format!("bd recall {key}")), "{told}");

    let (_, mems) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{pid}/beads/memories"),
        None,
    )
    .await;
    assert_eq!(mems[0]["key"], key);
    assert_eq!(
        mems[0]["value"],
        "tmux misses pane exits; run-shell true reaps them"
    );
    assert_eq!(mems[0]["evidence"]["task_title"], "Fix #42");

    // "All my projects" goes to user-level memory, not Beads.
    let (status, shared) = call(
        &h.app,
        "POST",
        &format!("{uri}/{b}:accept"),
        Some(json!({"scope": "user"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{shared}");
    assert!(shared["entry"]["beads_key"].is_null());
    let (_, user) = call(&h.app, "GET", "/v1/memory", None).await;
    assert_eq!(user[0]["text"], "Run the app's e2e suite serially on CI");
    let (_, mems) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{pid}/beads/memories"),
        None,
    )
    .await;
    assert_eq!(mems.as_array().unwrap().len(), 1);

    let forget = format!("/v1/projects/{pid}/beads/memories/{key}");
    let (status, _) = call(&h.app, "DELETE", &forget, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&h.app, "DELETE", &forget, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// `bd --json` in the Project's database.
fn bd_json(h: &Harness, pid: &str, args: &[&str]) -> Value {
    let out = std::process::Command::new(bd())
        .args(args)
        .arg("--json")
        .current_dir(quarkd::beads::dir_of(h.home.path(), pid))
        .env("BD_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "bd {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn bead(h: &Harness, pid: &str, id: &str) -> Value {
    let v = bd_json(h, pid, &["show", id]);
    v.as_array().map_or(v.clone(), |a| a[0].clone())
}

fn hold(id: &str, question: &str) -> Hold {
    Hold {
        id: id.into(),
        task_id: None,
        question: question.into(),
        answer: None,
        answered_by: None,
        brief: serde_json::from_value(json!({
            "context": "The new log page is ready.",
            "options": [{"label": "Behind a flag", "consequence": "Nobody sees it yet"}, "Ship it"],
            "recommended": "Behind a flag"
        }))
        .unwrap(),
    }
}

async fn decision(h: &Harness, id: &str) -> Value {
    let (status, d) = call(&h.app, "GET", &format!("/v1/decisions/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{d}");
    d
}

#[tokio::test]
async fn decisions_are_beads_with_gates_and_either_side_answers() {
    if !installed() {
        eprintln!("skipped: bd and dolt are not both installed");
        return;
    }
    let h = harness();
    let pid = ready_project(&h).await;
    // Before the database exists there is nothing to mirror into.
    api::reconcile_decision_beads(&h.state, &pid).await.unwrap();
    set_up(&h, &pid).await;

    h.engine.set_holds(vec![
        hold("flag", "Ship the log page behind a flag?"),
        hold("copy", "Which wording for the empty state?"),
    ]);
    h.projector.refresh_all().await.unwrap();
    let (_, open) = call(&h.app, "GET", "/v1/decisions?state=open", None).await;
    let id_of = |q: &str| {
        open.as_array()
            .unwrap()
            .iter()
            .find(|d| d["question"] == q)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (flag, copy) = (
        id_of("Ship the log page behind a flag?"),
        id_of("Which wording for the empty state?"),
    );

    // Each open decision becomes a decision bead blocked by a human gate.
    api::reconcile_decision_beads(&h.state, &pid).await.unwrap();
    let flag_bead = decision(&h, &flag).await["bead_id"]
        .as_str()
        .unwrap()
        .to_string();
    let copy_bead = decision(&h, &copy).await["bead_id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = bead(&h, &pid, &flag_bead);
    assert_eq!(b["issue_type"], "decision");
    assert_eq!(b["status"], "open");
    assert_eq!(b["external_ref"], format!("quark:{flag}"));
    assert!(
        b["description"]
            .as_str()
            .unwrap()
            .contains("**Behind a flag** (recommended)"),
        "{b}"
    );
    let gate = b["metadata"]["gate"].as_str().unwrap().to_string();
    assert_eq!(bead(&h, &pid, &gate)["status"], "open");
    // A second pass changes nothing.
    api::reconcile_decision_beads(&h.state, &pid).await.unwrap();
    let decisions = bd_json(
        &h,
        &pid,
        &["list", "--all", "--type", "decision", "--limit", "0"],
    );
    assert_eq!(decisions.as_array().unwrap().len(), 2);

    // Answered in Quark: the gate resolves and the bead closes with the
    // answer; the rule it made becomes a memory agents see.
    let (status, d) = call(
        &h.app,
        "POST",
        &format!("/v1/decisions/{flag}:answer"),
        Some(json!({"answer": "Behind a flag", "answered_by": "matt", "make_rule": "New pages ship behind a flag"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{d}");
    let rule = d["made_rule_id"].as_str().unwrap().to_string();
    api::reconcile_decision_beads(&h.state, &pid).await.unwrap();
    let b = bead(&h, &pid, &flag_bead);
    assert_eq!(b["status"], "closed");
    assert_eq!(b["close_reason"], "Behind a flag");
    assert!(
        b["labels"]
            .as_array()
            .unwrap()
            .contains(&json!("standing-rule")),
        "{b}"
    );
    assert_eq!(bead(&h, &pid, &gate)["status"], "closed");
    let key = format!("quark-rule-{rule}");
    let memories = bd_json(&h, &pid, &["memories"]);
    assert!(
        memories[&key]
            .as_str()
            .unwrap()
            .ends_with("New pages ship behind a flag"),
        "{memories}"
    );

    // Answered in Beads by resolving the gate: Quark records the answer,
    // the asker gets it, and the bead closes.
    let gate = bead(&h, &pid, &copy_bead)["metadata"]["gate"]
        .as_str()
        .unwrap()
        .to_string();
    let out = std::process::Command::new(bd())
        .args(["gate", "resolve", &gate, "--reason", "Nothing here yet"])
        .current_dir(quarkd::beads::dir_of(h.home.path(), &pid))
        .env("BD_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    api::reconcile_decision_beads(&h.state, &pid).await.unwrap();
    let d = decision(&h, &copy).await;
    assert_eq!(d["state"], "answered");
    assert_eq!(d["answer"], "Nothing here yet");
    assert_eq!(d["answered_via"], "beads");
    assert!(h.engine.writes().iter().any(|w| matches!(w,
        StubWrite::Answer { hold_id, answer, .. } if hold_id == "copy" && answer == "Nothing here yet")));
    api::reconcile_decision_beads(&h.state, &pid).await.unwrap();
    let b = bead(&h, &pid, &copy_bead);
    assert_eq!(b["status"], "closed");
    assert_eq!(b["close_reason"], "Nothing here yet");

    // What the asker did is noted once.
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/decisions/{flag}:act"),
        Some(json!({"outcome": "Merged #101"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    api::reconcile_decision_beads(&h.state, &pid).await.unwrap();
    api::reconcile_decision_beads(&h.state, &pid).await.unwrap();
    let notes = bead(&h, &pid, &flag_bead)["notes"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        notes.matches("What happened: Merged #101").count(),
        1,
        "{notes}"
    );

    // Changing the rule rewrites its memory; revoking forgets it.
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/rules/{rule}:change"),
        Some(json!({"text": "New pages ship behind a flag for a week"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    api::reconcile_decision_beads(&h.state, &pid).await.unwrap();
    let memories = bd_json(&h, &pid, &["memories"]);
    assert!(
        memories[&key].as_str().unwrap().ends_with("for a week"),
        "{memories}"
    );
    let (status, _) = call(&h.app, "POST", &format!("/v1/rules/{rule}:revoke"), None).await;
    assert_eq!(status, StatusCode::OK);
    api::reconcile_decision_beads(&h.state, &pid).await.unwrap();
    let memories = bd_json(&h, &pid, &["memories"]);
    assert!(memories.get(&key).is_none(), "{memories}");
}
