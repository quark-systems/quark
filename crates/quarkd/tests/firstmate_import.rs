//! Importing a firstmate home: the plan a dry run answers, and a Project
//! seeded from the home once it is provisioned (dispatch rules,
//! instructions, memory, library files, user-level memory, quota settings
//! and the backlog as beads).
//!
//! The beads half drives the real `bd` (and `dolt`); where they are not
//! installed it checks the rest and says so. Set `QUARK_BD` to use a `bd`
//! that is not on `PATH`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use quarkd::api::{self, AppState};
use quarkd::beads::Beads;
use quarkd::chat::RecordingInput;
use quarkd::engine::StubEngine;
use quarkd::harness::{HarnessRegistry, HostEnv};
use quarkd::provision::Layout;
use quarkd::store::Store;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Harness {
    home: tempfile::TempDir,
    fm: tempfile::TempDir,
    app: Router,
}

impl Drop for Harness {
    /// Server mode leaves a Dolt sql-server running for the database.
    fn drop(&mut self) {
        let Ok(dirs) = std::fs::read_dir(self.home.path().join("beads")) else {
            return;
        };
        for dir in dirs.flatten() {
            let _ = Command::new(bd())
                .args(["dolt", "stop"])
                .current_dir(dir.path())
                .output();
        }
    }
}

fn bd() -> String {
    std::env::var("QUARK_BD").unwrap_or_else(|_| "bd".into())
}

fn installed() -> bool {
    let runs = |bin: &str| {
        Command::new(bin)
            .arg("version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    runs(&bd()) && runs("dolt")
}

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A firstmate home with two projects (one cloned), a backlog in every
/// section, preferences, learnings, a brief and dispatch rules.
fn firstmate_home(root: &Path) {
    write(
        root,
        "data/projects.md",
        "# Projects\n\n\
         - alpha [direct-PR] - The alpha service (added 2026-07-16)\n\
         - beta [no-mistakes] - Not cloned here (added 2026-08-01)\n",
    );
    write(
        root,
        "data/backlog.md",
        "# Backlog\n\n## In flight\n\
         - [ ] live - Live work (repo: alpha) (kind: ship) (since 2026-10-01)\n\
         ## Queued\n\
         - [ ] next - Next work blocked-by: live (repo: alpha) (kind: ship) (priority: 1)\n\
         - [ ] call - Pick a host (repo: alpha) (kind: captain) (hold: which host runs it) (hold-kind: captain)\n\
         \x20 Captain hold set: 2026-10-02T00:00:00Z\n\
         ## Done\n\
         - [x] shipped - Shipped https://github.com/o/alpha/pull/2 (repo: alpha) (kind: ship) (merged 2026-08-25)\n\
         \x20 PR #2 merged by captain.\n",
    );
    write(root, "data/shipped/brief.md", "# Task\nShip the thing.\n");
    write(
        root,
        "data/captain.md",
        "<!-- memory tiers: see the stow skill -->\n- Keep PRs small <!--P-->\n",
    );
    write(
        root,
        "data/learnings.md",
        "- The alpha tests need Docker <!--a:2026-08-01-->\n",
    );
    write(
        root,
        "config/crew-dispatch.json",
        r#"{"rules":[{"when":"Trivial edits","use":[{"harness":"claude","model":"claude-sonnet-5","effort":"low"}]}],
            "default":[{"harness":"claude","model":"claude-sonnet-5","effort":"medium"}]}"#,
    );
    write(root, "config/crew-harness", "claude\n");
    write(root, "config/bob-quota.json", r#"{"monthly_coins":900}"#);
    write(root, "config/calm", "on\n");
    write(root, ".env", "FMX_PAIRING_TOKEN=secret\n");
    write(root, "state/live.status", "working: started\n");
    write(root, "state/live.meta", "window=fm:live\n");
    let clone = root.join("projects/alpha");
    std::fs::create_dir_all(&clone).unwrap();
    git(&clone, &["init", "--quiet"]);
    git(
        &clone,
        &["remote", "add", "origin", "git@github.com:o/alpha.git"],
    );
}

fn harness() -> Harness {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let home = tempfile::tempdir().unwrap();
    let fm = tempfile::tempdir().unwrap();
    firstmate_home(fm.path());
    let harnesses = Arc::new(HarnessRegistry::new(
        quarkd::harness::builtin(),
        HostEnv::default(),
    ));
    let state = AppState {
        store: store.clone(),
        engine,
        harnesses: harnesses.clone(),
        accounts: Arc::new(quarkd::accounts::Accounts::new(
            store,
            harnesses,
            Arc::new(quarkd::accounts::StubQuota::new()),
            &["CLAUDE_CONFIG_DIR"],
        )),
        sessions: quarkd::sessions::Sessions::disabled("not used in this test"),
        layout: Layout::new(home.path()).with_user_memory(Some(home.path().join("shared"))),
        chat: Arc::new(RecordingInput::new()),
        forge: Arc::new(quarkd::forge::StubForge::new()),
        events: quark_eventlog::SqliteEventLog::open(":memory:").unwrap(),
        triggers: None,
        beads: Arc::new(
            Beads::new(Some(bd().into()), "http://127.0.0.1:7380".into()).with_auto_setup(),
        ),
    };
    Harness {
        home,
        fm,
        app: api::router(state),
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

fn show(bare: &Path, rel: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(bare)
        .args(["show", &format!("main:{rel}")])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn item<'a>(plan: &'a Value, what: &str) -> &'a Value {
    plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["what"] == what)
        .unwrap_or_else(|| panic!("no item {what}: {plan}"))
}

#[tokio::test]
async fn a_dry_run_plans_and_creates_nothing() {
    let h = harness();
    let path = h.fm.path().to_string_lossy();
    let (status, body) = call(
        &h.app,
        "POST",
        "/v1/imports/firstmate",
        Some(json!({ "path": path, "dry_run": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["project"].is_null());
    let plan = &body["plan"];
    let project = &plan["project"];
    assert_eq!(project["name"], "alpha");
    assert_eq!(project["repos"][0]["url"], "git@github.com:o/alpha.git");
    assert_eq!(project["repos"].as_array().unwrap().len(), 1);
    assert_eq!(project["delivery"], "direct");
    assert_eq!(project["agent_config"]["harness"], "claude-code");
    assert_eq!(project["agent_config"]["model"], "claude-sonnet-5");
    assert_eq!(plan["issues"], 4);
    assert_eq!(item(plan, "Backlog")["count"], 4);
    assert_eq!(item(plan, "Calls waiting on you")["count"], 1);
    assert_eq!(item(plan, "Secrets")["outcome"], "skipped");
    assert_eq!(item(plan, "Running workers")["outcome"], "skipped");
    let warnings = plan["warnings"].to_string();
    assert!(warnings.contains("beta"), "{warnings}");
    assert!(warnings.contains("being worked on"), "{warnings}");

    let (_, projects) = call(&h.app, "GET", "/v1/projects", None).await;
    assert_eq!(projects.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn an_import_seeds_the_project() {
    let h = harness();
    let path = h.fm.path().to_string_lossy();
    let (status, body) = call(
        &h.app,
        "POST",
        "/v1/imports/firstmate",
        Some(json!({ "path": path, "name": "From firstmate" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["project"]["id"].as_str().unwrap().to_string();
    let bare: PathBuf = h.home.path().join("projects").join(format!("{id}.git"));

    let mut report = None;
    for _ in 0..300 {
        report = show(&bare, "library/firstmate/IMPORT.md");
        if report.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let report = report.expect("the import records IMPORT.md");
    assert!(report.contains("| Backlog"), "{report}");

    let dispatch = show(&bare, "dispatch.yaml").unwrap();
    assert!(dispatch.contains("Trivial edits"), "{dispatch}");
    assert!(dispatch.contains("harness: \"claude-code\""), "{dispatch}");
    let instructions = show(&bare, "instructions.md").unwrap();
    assert!(
        instructions.contains("`alpha` (direct-PR)"),
        "{instructions}"
    );
    assert_eq!(
        show(&bare, "library/firstmate/tasks/shipped/brief.md").as_deref(),
        Some("# Task\nShip the thing.\n")
    );
    assert!(show(&bare, "library/firstmate/tasks/live/status.log").is_some());
    assert!(show(&bare, "library/firstmate/backlog.md").is_some());

    let (_, memory) = call(&h.app, "GET", &format!("/v1/projects/{id}/memory"), None).await;
    assert!(
        memory.to_string().contains("The alpha tests need Docker"),
        "{memory}"
    );
    let shared: Vec<String> = std::fs::read_dir(h.home.path().join("shared"))
        .unwrap()
        .flatten()
        .map(|f| std::fs::read_to_string(f.path()).unwrap())
        .collect();
    assert!(shared.iter().any(|s| s.contains("Keep PRs small")));
    assert_eq!(
        std::fs::read_to_string(h.home.path().join("quota/bob.json")).unwrap(),
        r#"{"monthly_coins":900}"#
    );
    assert!(!report.contains("secret"));

    if !installed() {
        eprintln!("bd or dolt is not installed; skipping the backlog's beads");
        return;
    }
    assert!(
        report.contains("4 of 4 backlog tasks are beads"),
        "{report}"
    );
    let (status, issues) = call(&h.app, "GET", &format!("/v1/projects/{id}/issues"), None).await;
    assert_eq!(status, StatusCode::OK, "{issues}");
    let issues = issues.as_array().unwrap();
    let by = |title: &str| {
        issues
            .iter()
            .find(|i| i["title"] == title)
            .unwrap_or_else(|| panic!("no bead {title}: {issues:?}"))
    };
    assert_eq!(by("Live work")["status"], "in_progress");
    assert_eq!(by("Next work")["status"], "open");
    assert_eq!(by("Shipped")["status"], "closed");
    assert!(by("Shipped")["labels"]
        .as_array()
        .unwrap()
        .contains(&json!("firstmate")));
    let next = by("Next work")["id"].as_str().unwrap();
    let (_, detail) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{id}/issues/{next}"),
        None,
    )
    .await;
    assert!(detail.to_string().contains("Live work"), "{detail}");
    let shipped = by("Shipped")["id"].as_str().unwrap();
    let (_, detail) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{id}/issues/{shipped}"),
        None,
    )
    .await;
    assert!(detail["design"]
        .as_str()
        .unwrap()
        .contains("Ship the thing"));
}

#[tokio::test]
async fn not_a_firstmate_home_is_refused() {
    let h = harness();
    let empty = tempfile::tempdir().unwrap();
    let (status, body) = call(
        &h.app,
        "POST",
        "/v1/imports/firstmate",
        Some(json!({ "path": empty.path().to_string_lossy(), "dry_run": true })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("not a firstmate home"));
    let (status, _) = call(
        &h.app,
        "POST",
        "/v1/imports/firstmate",
        Some(json!({ "path": "relative/home", "dry_run": true })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
