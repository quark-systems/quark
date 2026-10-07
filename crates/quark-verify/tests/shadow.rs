//! Slice 2 shadow: firstmate's recorded decisions replayed against native.
//! The lines below are in the exact shape firstmate's
//! `bin/fm-main-health-lib.sh` writes (schema `fm.guard.v1`).

use std::sync::Arc;

use quark_core::{EventLog, ProjectId, Seq};
use quark_verify::health::CheckRun;
use quark_verify::shadow::MemoryCheckpointLog;
use quark_verify::{kinds, FakeForge, ShadowCheck, ShadowVerifier};

const TIP: &str = "1111111111111111111111111111111111111111";
const HEAD: &str = "7373737373737373737373737373737373737373";

fn merge(reason: &str, permit: &str, fresh: &str, main_status: &str, green: &str) -> String {
    format!(
        r#"{{"schema":"fm.guard.v1","ts":"2026-10-07T03:15:24Z","op":"merge","task":"t1","pr":"https://github.com/acme/app/pull/70","repo":"acme/app","base":"main","head":"{HEAD}","fresh":"{fresh}","tip":"{TIP}","behind":0,"gates":{{"required":false,"evidence_head":"","evidence_state":""}},"main":{{"status":"{main_status}","read":"red","tip":"{TIP}","commit":"{TIP}","checks":["build"],"decision":"d1"}},"pr_green":[{green}],"permit":"{permit}","reason":"{reason}"}}"#
    )
}

fn dispatch(
    task: &str,
    fix_main: bool,
    prior: &str,
    live: bool,
    permit: &str,
    reason: &str,
) -> String {
    format!(
        r#"{{"schema":"fm.guard.v1","ts":"2026-10-07T03:15:24Z","op":"dispatch","task":"{task}","repo":"acme/app","base":"main","fix_main":{fix_main},"main":{{"status":"red","read":"red","tip":"{TIP}","commit":"{TIP}","checks":["build"],"decision":"d1"}},"prior_fix_task":"{prior}","prior_fix_live":{live},"permit":"{permit}","reason":"{reason}"}}"#
    )
}

fn home_with(lines: &[String]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    append(dir.path(), lines);
    dir
}

fn append(home: &std::path::Path, lines: &[String]) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("state/guard-decisions.jsonl"))
        .unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

async fn checks(log: &MemoryCheckpointLog) -> Vec<ShadowCheck> {
    log.log
        .read(Seq::ZERO, 1000)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind.as_str() == kinds::SHADOW)
        .map(|e| e.decode().unwrap())
        .collect()
}

async fn divergences(log: &MemoryCheckpointLog) -> Vec<String> {
    log.log
        .read(Seq::ZERO, 1000)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind.as_str() == "shadow.divergence")
        .map(|e| e.payload["operation"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn native_rules_agree_with_firstmates_decisions() {
    let home = home_with(&[
        merge("main_red", "deny", "fresh", "red", ""),
        merge("fixes_main", "allow", "fresh", "red", r#""build","ci""#),
        merge("overridden", "allow", "fresh", "overridden", ""),
        merge("stale_head", "deny", "stale", "red", ""),
        dispatch("work-b", false, "", false, "deny", "main_red"),
        dispatch("fix-a", true, "", false, "allow", "fix_main_claimed"),
        dispatch("fix-b", true, "fix-a", true, "deny", "fix_main_taken"),
        dispatch("fix-c", true, "fix-a", false, "allow", "fix_main_claimed"),
        r#"{"schema":"fm.guard.v1","op":"merge","task":"t","repo":"acme/app","permit":"deny","reason":"red_main_record_failed"}"#.into(),
        "not json".into(),
    ]);
    let log = Arc::new(MemoryCheckpointLog::new());
    let shadow = ShadowVerifier::new(log.clone(), None, "h".into());
    let p = ProjectId::from("p");
    let r = shadow.ingest(&p, home.path()).await.unwrap();
    assert_eq!((r.decisions, r.divergences, r.unparsed), (9, 0, 1));
    let c = checks(&log).await;
    assert_eq!(c.len(), 9);
    assert!(c.iter().take(8).all(|c| c.native.as_ref() == Some(&c.bash)));
    // firstmate's own write failure has nothing to replay.
    assert!(c[8].native.is_none());

    // Nothing new: nothing compared again.
    let r = shadow.ingest(&p, home.path()).await.unwrap();
    assert_eq!(r.decisions, 0);
    assert_eq!(checks(&log).await.len(), 9);
}

#[tokio::test]
async fn rule_disagreements_are_recorded_once() {
    // firstmate let an unrelated PR onto a red main: native would refuse.
    let home = home_with(&[merge("main_clear", "allow", "fresh", "red", "")]);
    let log = Arc::new(MemoryCheckpointLog::new());
    let shadow = ShadowVerifier::new(log.clone(), None, "h".into());
    let p = ProjectId::from("p");
    let r = shadow.ingest(&p, home.path()).await.unwrap();
    assert_eq!(r.divergences, 1);
    assert_eq!(divergences(&log).await, vec!["may_merge"]);

    // A later line is compared on the next pass, the old one is not.
    append(
        home.path(),
        &[dispatch("w", false, "", false, "deny", "main_red")],
    );
    let r = shadow.ingest(&p, home.path()).await.unwrap();
    assert_eq!((r.decisions, r.divergences), (1, 0));
    assert_eq!(divergences(&log).await.len(), 1);

    // A fresh verifier on the same log resumes from the checkpoint.
    let again = ShadowVerifier::new(log.clone(), None, "h".into());
    assert_eq!(again.ingest(&p, home.path()).await.unwrap().decisions, 0);
}

#[tokio::test]
async fn native_reads_check_firstmates_reads() {
    let forge = Arc::new(FakeForge::new());
    forge.set_behind(TIP, HEAD, 0);
    forge.set_checks(
        TIP,
        vec![CheckRun::new(1, "build", "completed", Some("failure"))],
        vec![],
    );
    forge.set_checks(
        HEAD,
        vec![
            CheckRun::new(2, "build", "completed", Some("success")),
            CheckRun::new(3, "ci", "completed", Some("success")),
        ],
        vec![],
    );
    let home = home_with(&[merge(
        "fixes_main",
        "allow",
        "fresh",
        "red",
        r#""build","ci""#,
    )]);
    let log = Arc::new(MemoryCheckpointLog::new());
    let shadow = ShadowVerifier::new(log.clone(), Some(forge.clone()), "h".into());
    let p = ProjectId::from("p");
    let r = shadow.ingest(&p, home.path()).await.unwrap();
    assert_eq!(r.divergences, 0);
    let c = &checks(&log).await[0];
    assert_eq!(
        c.compared,
        vec!["may_merge", "head_fresh", "main_health", "pr_green"]
    );
    assert!(c.unverified.is_empty());

    // The forge says otherwise on every read.
    forge.set_behind(TIP, HEAD, 3);
    forge.set_checks(
        TIP,
        vec![CheckRun::new(1, "build", "completed", Some("success"))],
        vec![],
    );
    forge.set_checks(HEAD, vec![], vec![]);
    append(
        home.path(),
        &[merge("fixes_main", "allow", "fresh", "red", r#""build""#)],
    );
    let r = shadow.ingest(&p, home.path()).await.unwrap();
    assert_eq!(r.divergences, 3);
    assert_eq!(
        divergences(&log).await,
        vec!["head_fresh", "main_health", "pr_green"]
    );
}

#[tokio::test]
async fn unreadable_native_reads_are_unverified_not_divergent() {
    let forge = Arc::new(FakeForge::new());
    let home = home_with(&[merge("main_red", "deny", "fresh", "red", "")]);
    let log = Arc::new(MemoryCheckpointLog::new());
    let shadow = ShadowVerifier::new(log.clone(), Some(forge), "h".into());
    let r = shadow
        .ingest(&ProjectId::from("p"), home.path())
        .await
        .unwrap();
    assert_eq!(r.divergences, 0);
    let c = &checks(&log).await[0];
    assert_eq!(c.unverified, vec!["head_fresh", "main_health", "pr_green"]);
}

#[tokio::test]
async fn no_file_is_nothing_to_do() {
    let home = tempfile::tempdir().unwrap();
    let log = Arc::new(MemoryCheckpointLog::new());
    let shadow = ShadowVerifier::new(log, None, "h".into());
    let r = shadow
        .ingest(&ProjectId::from("p"), home.path())
        .await
        .unwrap();
    assert_eq!(r.decisions, 0);
}
