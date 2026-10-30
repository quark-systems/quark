//! Importing a firstmate home: what is read, what lands in Quark, that a
//! second import changes nothing, and that the home itself is never touched.
//!
//! The home is built here from invented data. The Beads half drives the real
//! `bd` and `dolt` and is skipped where they are not installed.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

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

const SECRET: &str = "TOP-SECRET-TOKEN-VALUE";

fn bd() -> String {
    std::env::var("QUARK_BD").unwrap_or_else(|_| "bd".into())
}

fn installed() -> bool {
    let runs = |bin: &str| {
        std::process::Command::new(bin)
            .arg("version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    runs(&bd()) && runs("dolt")
}

struct Harness {
    home: tempfile::TempDir,
    app: Router,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let Ok(dirs) = std::fs::read_dir(self.home.path().join("beads")) else {
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

fn harness() -> Harness {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(StubEngine::new());
    let home = tempfile::tempdir().unwrap();
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
        compaction: Default::default(),
        coordinator: None,
        beads: Arc::new(
            Beads::new(Some(bd().into()), "http://127.0.0.1:7380".into()).with_auto_setup(),
        ),
    };
    Harness {
        home,
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

fn put(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// A firstmate home with two projects and one of each kind of record.
fn firstmate_home() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    put(
        r,
        "data/projects.md",
        "# Projects\n\n- widgets [direct-PR] - The widget service (added 2026-01-02)\n- gadgets [no-mistakes] - Gadget tools\n",
    );
    for (name, url) in [
        (
            "widgets",
            "https://user:hunter2@example.test/acme/widgets.git",
        ),
        ("gadgets", "git@example.test:acme/gadgets.git"),
    ] {
        put(
            r,
            &format!("projects/{name}/.git/config"),
            &format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {url}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n"),
        );
    }
    put(
        r,
        "config/crew-dispatch.json",
        r#"{"classifier":{"provider":"none"},"rules":[{"when":"a trivial edit","select":"ordered","why":"cheap","use":[{"harness":"claude","model":"sonnet","effort":"low"},{"harness":"kiro","provider":"kiro"}]}],"default":{"harness":"claude","effort":"high"}}"#,
    );
    put(r, "config/crew-dispatch-alt.json", "{\"rules\":[]}");
    put(r, "config/kiro-quota.json", "{\"overage_allowed\": true}");
    put(r, "config/backend", "herdr");
    put(r, "config/crew-harness", "codex\n");
    put(
        r,
        "state/bob-quota-anchor.json",
        r#"{"recordedAt":"2020-01-01T00:00:00Z","usedCoins":9,"budgetCoins":900}"#,
    );
    put(
        r,
        "data/backlog.md",
        "## Done\n\
- [x] widgets-fix - Fix the widget https://example.test/acme/widgets/pull/9 (kind: ship) (merged 2026-02-03)\n\
## In flight\n\
- [ ] widgets-scout - Scout the cache (kind: scout) (since 2026-02-01)\n  Direction from the owner.\n\
## Queued\n\
- [ ] gadgets-pick - Pick a database (repo: gadgets) (kind: captain) (since 2026-02-01) (hold: A or B? (cost matters)) (hold-kind: captain)\n  Origin: gadgets-x\n\
- [ ] gadgets-old - Answered question (repo: gadgets) (kind: captain) (since 2026-01-01) (hold: Which one?)\n  Resolution recorded by fm-captain-hold.\n  Captain decision:\n  Option A.\n\
- [ ] widgets-next - Next thing blocked-by: widgets-fix (kind: ship) (priority: 1)\n\
- [ ] stray-idea - Something with no project (kind: ship)\n",
    );
    put(r, "data/widgets-scout/report.md", "# Report\n\nFindings.\n");
    put(
        r,
        "state/widgets-scout.status",
        "working: reading\ndone: report ready\n",
    );
    put(
        r,
        "state/widgets-scout.meta",
        "window=default:w1:p2\nharness=claude\nmodel=sonnet\nworktree=/private/tmp/x\nkind=scout\n",
    );
    put(r, "state/orphan.status", "working: nothing to attach to\n");
    put(
        r,
        "state/widgets-scout.check.sh",
        "#!/bin/sh\ntouch /tmp/MUST-NOT-RUN\n",
    );
    put(
        r,
        "data/captain.md",
        "# Captain preferences\n\n- Runtime preference (2026-07-29): use the fast backend.\n- Keep reviews short.\n",
    );
    put(
        r,
        "data/learnings.md",
        "# Fleet learnings\n\n## 2026-09-30 — A gotcha\n\nIt truncates history.\n",
    );
    put(r, ".env", &format!("API_TOKEN={SECRET}\n"));
    dir
}

/// Every file's path and bytes, for comparing a home before and after.
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.insert(
                    format!("{}/", p.strip_prefix(root).unwrap().display()),
                    vec![],
                );
                walk(&p, root, out);
            } else {
                out.insert(
                    p.strip_prefix(root).unwrap().display().to_string(),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn project<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("no project {name} in {report}"))
}

fn category<'a>(report: &'a Value, category: &str, source_has: &str) -> &'a Value {
    report["inventory"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["category"] == category && i["source"].as_str().unwrap().contains(source_has))
        .unwrap_or_else(|| panic!("no {category} item for {source_has}"))
}

#[tokio::test]
async fn bad_homes_are_refused() {
    let h = harness();
    let empty = tempfile::tempdir().unwrap();
    for (home, why) in [
        ("relative/path".to_string(), "absolute"),
        (empty.path().display().to_string(), "not a firstmate home"),
    ] {
        let (status, e) = call(
            &h.app,
            "POST",
            "/v1/imports/firstmate",
            Some(json!({"home": home})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{e}");
        assert!(e["error"]["message"].as_str().unwrap().contains(why), "{e}");
    }
    let fm = firstmate_home();
    let (status, e) = call(
        &h.app,
        "POST",
        "/v1/imports/firstmate",
        Some(json!({"home": fm.path(), "projects": ["nope"]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{e}");
}

#[tokio::test]
async fn dry_run_reports_everything_and_changes_nothing() {
    let h = harness();
    let fm = firstmate_home();
    let before = snapshot(fm.path());
    let quark_before = snapshot(h.home.path());
    let (status, r) = call(
        &h.app,
        "POST",
        "/v1/imports/firstmate",
        Some(json!({"home": fm.path(), "dry_run": true, "routes": {"widgets-": "widgets"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["dry_run"], true);
    assert_eq!(project(&r, "widgets")["project"], "created");
    assert_eq!(
        project(&r, "widgets")["repo_url"],
        "https://example.test/acme/widgets.git"
    );
    assert_eq!(project(&r, "widgets")["tasks"]["imported"], 3);
    assert_eq!(project(&r, "gadgets")["holds"]["imported"], 2);
    assert_eq!(r["unrouted"].as_array().unwrap().len(), 1);
    assert!(r["unrouted"][0].as_str().unwrap().starts_with("stray-idea"));
    // The classifier, variants, backend and secrets are accounted for.
    assert_eq!(category(&r, "dispatch", "crew-dispatch.json")["count"], 1);
    assert_eq!(
        category(&r, "dispatch", "crew-dispatch-*")["outcome"],
        "not_imported"
    );
    assert_eq!(category(&r, "runtime", "config/backend")["count"], 1);
    assert_eq!(category(&r, "runtime", "check.sh")["count"], 1);
    assert_eq!(category(&r, "secrets", ".env")["count"], 1);
    assert_eq!(category(&r, "memory", "captain.md")["count"], 2);
    assert_eq!(category(&r, "memory", "learnings.md")["count"], 1);
    assert_eq!(category(&r, "status", "no backlog item")["count"], 1);
    // codex can't run a native coordinator yet, so claude-code does.
    let coordinator = category(&r, "coordinator", "crew-harness");
    assert_eq!(coordinator["outcome"], "not_imported", "{r}");
    assert!(
        coordinator["target"]
            .as_str()
            .unwrap()
            .contains("claude-code"),
        "{r}"
    );
    assert!(
        r["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("codex")),
        "{r}"
    );
    assert!(!r.to_string().contains(SECRET));
    assert!(!r.to_string().contains("hunter2"));
    // Nothing was written, to firstmate or to Quark.
    assert_eq!(snapshot(fm.path()), before);
    assert_eq!(snapshot(h.home.path()), quark_before);
    let (_, projects) = call(&h.app, "GET", "/v1/projects", None).await;
    assert_eq!(projects.as_array().unwrap().len(), 0);
    assert!(!Path::new("/tmp/MUST-NOT-RUN").exists());
}

#[tokio::test]
async fn a_crew_harness_quark_can_coordinate_with_runs_the_coordinator() {
    let h = harness();
    let fm = firstmate_home();
    put(fm.path(), "config/crew-harness", "claude\n");
    let (status, r) = call(
        &h.app,
        "POST",
        "/v1/imports/firstmate",
        Some(json!({"home": fm.path(), "dry_run": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    let coordinator = category(&r, "coordinator", "crew-harness");
    assert_eq!(coordinator["outcome"], "imported", "{r}");
    assert!(
        coordinator["target"]
            .as_str()
            .unwrap()
            .contains("claude-code"),
        "{r}"
    );
    assert!(
        !r["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("crew-harness")),
        "{r}"
    );
}

#[tokio::test]
async fn imports_then_a_second_import_adds_nothing_and_firstmate_is_untouched() {
    let h = harness();
    let fm = firstmate_home();
    let before = snapshot(fm.path());
    let req = json!({"home": fm.path(), "routes": {"widgets-": "widgets"}, "wait_secs": 60});
    let (status, r) = call(&h.app, "POST", "/v1/imports/firstmate", Some(req.clone())).await;
    assert_eq!(status, StatusCode::OK, "{r}");
    let widgets = project(&r, "widgets");
    assert_eq!(widgets["project"], "created", "{r}");
    // Dispatch lands whether or not Beads can run here.
    assert_eq!(widgets["dispatch"], "imported", "{r}");
    if !installed() {
        assert_eq!(widgets["outcome"], "pending", "{r}");
        assert!(widgets["detail"].as_str().unwrap().contains("Beads"), "{r}");
    }
    let id = widgets["project_id"].as_str().unwrap();

    // The project took its repo and delivery posture from firstmate.
    let (_, p) = call(&h.app, "GET", &format!("/v1/projects/{id}"), None).await;
    assert_eq!(p["delivery"], "direct");
    assert_eq!(
        p["repos"][0]["url"],
        "https://example.test/acme/widgets.git"
    );
    let (_, g) = call(&h.app, "GET", "/v1/projects", None).await;
    assert_eq!(g.as_array().unwrap().len(), 2);

    // Dispatch: harness ids are Quark's and the typed fields survived.
    let (_, rules) = call(&h.app, "GET", &format!("/v1/projects/{id}/dispatch"), None).await;
    assert_eq!(rules["rules"][0]["when"], "a trivial edit");
    assert_eq!(rules["rules"][0]["candidates"][0]["harness"], "claude-code");
    assert_eq!(rules["rules"][0]["candidates"][1]["provider"], "kiro");
    assert_eq!(rules["rules"][0]["why"], "cheap");
    assert_eq!(rules["default"][0]["harness"], "claude-code");

    // Quota and memory.
    let kiro = std::fs::read_to_string(h.home.path().join("quota/kiro.json")).unwrap();
    assert!(kiro.contains("true"));
    assert!(h.home.path().join("quota/bob.json").is_file());
    let (_, mem) = call(&h.app, "GET", "/v1/memory", None).await;
    assert_eq!(mem.as_array().unwrap().len(), 3, "{mem}");
    assert!(mem
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["text"].as_str().unwrap().contains("fast backend")));

    if installed() {
        assert_eq!(widgets["tasks"]["imported"], 3, "{widgets}");
        assert_eq!(project(&r, "gadgets")["holds"]["imported"], 2, "{r}");
        let (_, issues) = call(&h.app, "GET", &format!("/v1/projects/{id}/issues"), None).await;
        let issues = issues.as_array().unwrap();
        let by = |l: &str| {
            issues
                .iter()
                .find(|i| i["labels"].as_array().unwrap().iter().any(|x| x == l))
                .unwrap_or_else(|| panic!("no bead {l}: {issues:?}"))
        };
        assert_eq!(by("firstmate:widgets-fix")["status"], "closed");
        assert_eq!(by("firstmate:widgets-scout")["status"], "open");
        let next = by("firstmate:widgets-next");
        assert_eq!(next["priority"], 1);
        assert_eq!(next["blocked_by"][0], by("firstmate:widgets-fix")["id"]);
        let detail = call(
            &h.app,
            "GET",
            &format!(
                "/v1/projects/{id}/issues/{}",
                by("firstmate:widgets-scout")["id"].as_str().unwrap()
            ),
            None,
        )
        .await
        .1;
        let comments = detail["comments"].to_string();
        assert!(
            comments.contains("report ready") && comments.contains("Findings."),
            "{detail}"
        );
        assert!(
            !detail.to_string().contains("worktree")
                && !detail.to_string().contains("/private/tmp")
        );
        let gid = project(&r, "gadgets")["project_id"].as_str().unwrap();
        let (_, gi) = call(&h.app, "GET", &format!("/v1/projects/{gid}/issues"), None).await;
        let gi = gi.as_array().unwrap();
        let open = gi
            .iter()
            .find(|i| i["labels"].to_string().contains("gadgets-pick"))
            .unwrap();
        assert_eq!(open["issue_type"], "decision");
        assert_eq!(open["status"], "open");
        let answered = gi
            .iter()
            .find(|i| i["labels"].to_string().contains("gadgets-old"))
            .unwrap();
        assert_eq!(answered["status"], "closed");
    } else {
        eprintln!("bd or dolt is not installed: Beads import not checked");
    }

    // Importing again creates nothing.
    let (status, again) = call(&h.app, "POST", "/v1/imports/firstmate", Some(req)).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(project(&again, "widgets")["project"], "existing");
    assert_eq!(
        project(&again, "widgets")["project_id"],
        widgets["project_id"]
    );
    assert_eq!(project(&again, "widgets")["tasks"]["imported"], 0);
    assert_eq!(project(&again, "widgets")["dispatch"], "present");
    assert_eq!(project(&again, "gadgets")["holds"]["imported"], 0);
    assert_eq!(
        category(&again, "memory", "captain.md")["outcome"],
        "present"
    );
    let (_, g) = call(&h.app, "GET", "/v1/projects", None).await;
    assert_eq!(g.as_array().unwrap().len(), 2);
    let (_, mem) = call(&h.app, "GET", "/v1/memory", None).await;
    assert_eq!(mem.as_array().unwrap().len(), 3);

    // firstmate was only read, and its scripts never ran.
    assert_eq!(snapshot(fm.path()), before);
    assert!(!Path::new("/tmp/MUST-NOT-RUN").exists());
    assert!(!h.home.path().join("quota").join(".env").exists());
}
