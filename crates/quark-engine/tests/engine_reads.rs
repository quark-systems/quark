use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use quark_engine::holds::decisions;
use quark_engine::runner::MemoryCallLog;
use quark_engine::snapshot::{self, BacklogState, HoldBucket, Probe};
use quark_engine::status::DecisionKey;
use quark_engine::{summary, EngineReader, Workspace};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn real_snapshot_parses() {
    let s = snapshot::parse(&fs::read(fixture("fleet-snapshot.v1.json")).unwrap()).unwrap();
    assert_eq!(s.schema, snapshot::SCHEMA);
    let ids: Vec<_> = s.tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["external-wait", "mate", "scout-x", "ship-task"]);

    let ship = s.task("ship-task").unwrap();
    assert_eq!(ship.current_state.as_ref().unwrap().state, "working");
    assert_eq!(
        ship.pr.as_ref().unwrap().url.as_deref(),
        Some("https://github.com/kunchenguid/firstmate/pull/9")
    );
    assert_eq!(
        ship.endpoint.as_ref().unwrap().exists,
        Some(Probe::Known(true))
    );
    assert_eq!(
        ship.endpoint.as_ref().unwrap().agent_alive,
        Some(Probe::Word("not_checked".into()))
    );

    let mate = s.task("mate").unwrap();
    assert_eq!(mate.current_state.as_ref().unwrap().state, "parked");
    assert!(mate.hints.pending_decision);

    assert!(s.backlog.present);
    assert!(s.backlog_items().any(|r| r.state == BacklogState::Done));
    assert_eq!(s.scout_reports[0].id, "scout-x");
    assert!(s.main_inventory.as_ref().unwrap().valid);
}

#[test]
fn decisions_from_real_snapshot() {
    let s = snapshot::parse(&fs::read(fixture("fleet-snapshot.v1.json")).unwrap()).unwrap();
    let d = decisions(&s);

    let ids: Vec<_> = d
        .holds
        .iter()
        .map(|h| (h.task_id.as_str(), h.bucket))
        .collect();
    assert_eq!(
        ids,
        [
            ("pick-db", HoldBucket::Live),
            ("later-call", HoldBucket::Dated)
        ]
    );
    let live: Vec<_> = d.actionable_holds().map(|h| h.task_id.as_str()).collect();
    assert_eq!(live, ["pick-db"]);
    assert_eq!(d.holds[0].reason.as_deref(), Some("SQLite or DuckDB"));
    assert_eq!(d.holds[1].until.as_deref(), Some("2099-01-01"));

    assert_eq!(d.open.len(), 1);
    assert_eq!(d.open[0].task_id, "mate");
    assert_eq!(d.open[0].key, "race");
    assert_eq!(d.open[0].verb, "needs-decision");
}

#[test]
fn real_home_summary_parses() {
    let s = summary::read(&fixture("home-summary.v1.json"))
        .unwrap()
        .unwrap();
    assert_eq!(s.state.as_deref(), Some("captain_decision"));
    assert!(!s.valid);
    assert_eq!(s.counts.as_ref().unwrap().decisions_open, 2);
    let sources: Vec<_> = s
        .decisions_open
        .iter()
        .map(|d| (d.id.as_str(), d.source.as_deref()))
        .collect();
    assert_eq!(
        sources,
        [("pick-db", Some("backlog")), ("mate", Some("status"))]
    );
    assert_eq!(s.active_children[0].id, "ship-task");
}

/// A fake engine whose snapshot script prints the captured fixture.
fn fake_engine(dir: &Path) -> Workspace {
    let bin = dir.join("engine/bin");
    let state = dir.join("home/state");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&state).unwrap();
    let script = bin.join("fm-fleet-snapshot.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n[ \"$1\" = --json ] || exit 2\ncat '{}'\n",
            fixture("fleet-snapshot.v1.json").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    Workspace::new(dir.join("home"), dir.join("engine"))
}

#[test]
fn reader_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let ws = fake_engine(dir.path());
    let state = ws.state_dir();
    fs::copy(
        fixture("home-summary.v1.json"),
        state.join("home-summary.json"),
    )
    .unwrap();
    fs::write(
        state.join("ship-task.pr-poll"),
        "github\nhttps://github.com/kunchenguid/firstmate/pull/9\ngithub.com\nkunchenguid/firstmate\n9\n",
    )
    .unwrap();
    fs::write(
        state.join("mate.status"),
        "needs-decision [key=race]: pick subscribe order\ndone: an unrelated subtask finished\n",
    )
    .unwrap();

    let log = Arc::new(MemoryCallLog::default());
    let reader = EngineReader::new(ws, log.clone());

    let snap = reader.fleet_snapshot().unwrap();
    assert_eq!(snap.tasks.len(), 4);
    assert_eq!(log.calls().len(), 1);
    assert_eq!(log.calls()[0].script, "fm-fleet-snapshot.sh");

    assert!(reader.home_summary().unwrap().is_some());
    assert_eq!(reader.pr_poll("ship-task").unwrap().unwrap().number, 9);
    assert!(reader.pr_poll("scout-x").unwrap().is_none());
    assert!(reader.merge_notified("ship-task").unwrap().is_none());
    assert!(reader.pr_poll("../escape").is_err());

    let mut tail = reader.status_tail("mate").unwrap();
    let lines = tail.read_new().unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].event.key, DecisionKey::Stated("race".into()));
    assert!(lines[0].event.opens_decision());
}

/// Runs the real engine. Set FIRSTMATE_ROOT (a firstmate checkout) and
/// FM_HOME (a firstmate home), then `cargo test -- --ignored`.
#[test]
#[ignore]
fn live_engine_snapshot() {
    let root = std::env::var("FIRSTMATE_ROOT").expect("FIRSTMATE_ROOT");
    let home = std::env::var("FM_HOME").expect("FM_HOME");
    let reader = EngineReader::new(
        Workspace::new(home, root),
        Arc::new(MemoryCallLog::default()),
    );
    let snap = reader.fleet_snapshot().unwrap();
    assert_eq!(snap.schema, snapshot::SCHEMA);
    let _ = decisions(&snap);
}

#[test]
fn dispatch_resolution_and_spawn_meta() {
    let dir = tempfile::tempdir().unwrap();
    let ws = fake_engine(dir.path());
    // Echoes its arguments into the block, so the test sees exactly what ran.
    let script = dir.path().join("engine/bin/fm-dispatch-resolve.sh");
    fs::write(
        &script,
        "#!/bin/sh\n[ -r \"$1\" ] || exit 2\n\
         printf 'dispatch-resolve:\\n  status: escalate\\n  reason: args %s %s %s\\n' \"$(basename \"$1\")\" \"$2\" \"$3\"\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        ws.state_dir().join("t1.meta"),
        "harness=claude\nmodel=default\neffort=high\nspawn_gen=s1790000000.1.2\nproject=/h/projects/quark\n",
    )
    .unwrap();

    let log = Arc::new(MemoryCallLog::default());
    let reader = EngineReader::new(ws.clone(), log.clone());
    let meta = reader.spawn_meta("t1").unwrap().unwrap();
    assert_eq!(meta.harness, "claude");
    assert_eq!(meta.effort.as_deref(), Some("high"));
    assert!(reader.spawn_meta("t2").unwrap().is_none());

    // No brief yet: nothing runs.
    assert!(reader
        .dispatch_resolve("t1", Some("quark"))
        .unwrap()
        .is_none());
    assert!(log.calls().is_empty());

    fs::create_dir_all(ws.data_dir().join("t1")).unwrap();
    fs::write(ws.data_dir().join("t1/brief.md"), "fix the bug\n").unwrap();
    let r = reader
        .dispatch_resolve("t1", Some("quark"))
        .unwrap()
        .unwrap();
    assert_eq!(r.status, "escalate");
    assert_eq!(r.reason.as_deref(), Some("args brief.md --project quark"));
    assert_eq!(log.calls()[0].script, "fm-dispatch-resolve.sh");

    assert!(reader.dispatch_resolve("t1", Some("--evil")).is_err());
    assert!(reader.dispatch_resolve("../t1", None).is_err());
}
