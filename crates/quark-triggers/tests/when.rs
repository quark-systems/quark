//! Firstmate's when-then watches beside their native rules: the shadow
//! mirrors each watch as a rule, runs the same condition, and compares how
//! the two ended.
//!
//! `real_firstmate_watches_agree` arms and runs real watches with a
//! firstmate checkout: set FIRSTMATE_ROOT, then `cargo test -p
//! quark-triggers --test when -- --ignored`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use quark_core::slice::Divergence;
use quark_core::{EventLog, HostId, ProjectId, Seq};
use quark_eventlog::SqliteEventLog;
use quark_triggers::firstmate::{self, Verdict, WhenStatus};
use quark_triggers::{Engine, Mode, NoEffects, Processes};
use time::OffsetDateTime;

fn project() -> ProjectId {
    ProjectId::from("quark")
}

struct Rig {
    dir: tempfile::TempDir,
    log: Arc<SqliteEventLog>,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(SqliteEventLog::open(dir.path().join("events.db")).unwrap());
        std::fs::create_dir_all(dir.path().join("home/state/when")).unwrap();
        std::fs::create_dir_all(dir.path().join("home/state/procevent-inbox")).unwrap();
        Self { dir, log }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    async fn engine(&self) -> Engine {
        Engine::open(
            self.log.clone(),
            HostId::from("local"),
            Mode::Shadow,
            Arc::new(NoEffects),
            Arc::new(Processes),
        )
        .await
        .unwrap()
    }

    /// A spec as `fm-procevent-when.sh arm` writes it.
    fn arm(&self, name: &str, armed: i64, deadline: u64, cond: &[&str], act: &[&str]) {
        let mut s = format!(
            "fm-when-spec-v1\narmed={armed}\ninterval=0.5\nstable=2\ndeadline={deadline}\n\
             condition_timeout=5\naction_timeout=5\nerror_budget=2\naction_sha256=ab\n\
             condition_argc={}\naction_argc={}\nargv:\n",
            cond.len(),
            act.len()
        );
        for a in cond.iter().chain(act) {
            s.push_str(a);
            s.push('\n');
        }
        std::fs::write(self.home().join(format!("state/when/when-{name}.spec")), s).unwrap();
    }

    /// An outcome as the process-event runner captures it, at `at`.
    fn outcome(&self, name: &str, seq: u32, status: &str, at: OffsetDateTime) {
        let path = self
            .home()
            .join(format!("state/procevent-inbox/when-{name}.{seq}.result"));
        std::fs::write(
            &path,
            format!(
                "when: when-{name}\nstatus: {status}\ndetail: x\ncondition_polls: 2\noutput:\n"
            ),
        )
        .unwrap();
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(SystemTime::from(at)).unwrap();
    }

    fn retire(&self, name: &str) {
        let _ = std::fs::remove_file(self.home().join(format!("state/when/when-{name}.spec")));
    }

    async fn divergences(&self) -> Vec<Divergence> {
        self.log
            .read(Seq::ZERO, 10_000)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind.as_str() == "shadow.divergence")
            .map(|e| e.decode().unwrap())
            .collect()
    }
}

#[test]
fn parses_a_spec_like_firstmate() {
    let spec = firstmate::WhenSpec::parse(
        "fm-when-spec-v1\narmed=1760000000\ninterval=1.5\nstable=3\ndeadline=600\n\
         condition_timeout=10\naction_timeout=20\nerror_budget=4\naction_sha256=ab\n\
         condition_argc=2\naction_argc=1\nargv:\ntest\n-e /tmp/x\n/bin/true\n",
    )
    .unwrap();
    assert_eq!(spec.interval_secs, 2);
    assert_eq!(spec.condition, ["test", "-e /tmp/x"]);
    assert_eq!(spec.action, ["/bin/true"]);
    let rule = spec.rule("when-x");
    rule.validate().unwrap();
    let (armed, _) = firstmate::mirrored(&rule).unwrap();
    assert_eq!(armed.unix_timestamp(), 1_760_000_000);
    // Anything firstmate refuses is skipped.
    assert!(firstmate::WhenSpec::parse("fm-when-spec-v2\n").is_none());
    assert!(firstmate::WhenSpec::parse(
        "fm-when-spec-v1\narmed=1\ninterval=1\nstable=1\ndeadline=1\ncondition_timeout=1\n\
         action_timeout=1\nerror_budget=1\ncondition_argc=1\naction_argc=1\nargv:\nonly-one\n"
    )
    .is_none());
}

#[test]
fn verdicts() {
    let armed = OffsetDateTime::from_unix_timestamp(1_000).unwrap();
    let grace = time::Duration::minutes(5);
    let w = |status: Option<WhenStatus>| firstmate::Watch {
        source: "when-a".into(),
        spec: None,
        fired: false,
        outcomes: status
            .map(|s| firstmate::WhenOutcome {
                status: s,
                detail: String::new(),
                at: armed + time::Duration::seconds(10),
            })
            .into_iter()
            .collect(),
    };
    let held = firstmate::NativeEnd {
        held: Some(armed + time::Duration::seconds(5)),
        ..Default::default()
    };
    let none = firstmate::NativeEnd::default();
    let soon = armed + time::Duration::seconds(20);
    let late = armed + time::Duration::hours(1);
    let v = |watch, native, now| firstmate::compare_watch(&watch, armed, grace, native, now);
    assert_eq!(v(w(Some(WhenStatus::Fired)), held, soon), Verdict::Agree);
    assert_eq!(
        v(w(Some(WhenStatus::ActionFailed)), held, soon),
        Verdict::Agree
    );
    assert_eq!(v(w(Some(WhenStatus::Fired)), none, soon), Verdict::Pending);
    assert!(matches!(
        v(w(Some(WhenStatus::Fired)), none, late),
        Verdict::Diverge { .. }
    ));
    assert!(matches!(
        v(w(Some(WhenStatus::NeverTrue)), held, soon),
        Verdict::Diverge { .. }
    ));
    assert_eq!(v(w(Some(WhenStatus::Ambiguous)), held, soon), Verdict::Skip);
    assert_eq!(v(w(None), held, soon), Verdict::Pending);
    assert_eq!(
        v(w(None), held, late),
        Verdict::Diverge {
            bash: "none".into(),
            native: "fired".into()
        }
    );
}

#[tokio::test]
async fn mirrored_watches_agree_and_disagree() {
    let rig = Rig::new();
    let engine = rig.engine().await;
    let t0 = OffsetDateTime::now_utc();
    let armed = t0.unix_timestamp();
    // Holds for both: firstmate fired, native would have.
    rig.arm("ready", armed, 3600, &["true"], &["/bin/true"]);
    // Firstmate says it fired; the same condition never holds natively.
    rig.arm("split", armed, 3600, &["false"], &["/bin/true"]);
    // Never holds for either before its deadline.
    rig.arm("late", armed, 2, &["false"], &["/bin/true"]);

    let r = engine
        .mirror_firstmate_at(&project(), &rig.home(), t0)
        .await
        .unwrap();
    assert_eq!(r.watches, 3);
    let again = engine
        .mirror_firstmate_at(&project(), &rig.home(), t0)
        .await
        .unwrap();
    assert_eq!(again.watches, 0, "an unchanged watch is not redefined");

    // Two polls a second apart: `ready` holds; `late` passes its deadline.
    engine.tick(t0).await.unwrap();
    engine.tick(t0 + time::Duration::seconds(1)).await.unwrap();
    engine.tick(t0 + time::Duration::seconds(3)).await.unwrap();
    let id = |s: &str| quark_triggers::RuleId::new(s);
    assert_eq!(engine.fires(&project(), &id("when-ready")).await, 1);
    assert_eq!(engine.fires(&project(), &id("when-late")).await, 1);
    assert_eq!(engine.fires(&project(), &id("when-split")).await, 0);

    let at = t0 + time::Duration::seconds(2);
    rig.outcome("ready", 1, "fired", at);
    rig.outcome("split", 1, "fired", at);
    rig.outcome("late", 1, "never-true", at);
    for name in ["ready", "split", "late"] {
        rig.retire(name);
    }
    let r = engine
        .mirror_firstmate_at(&project(), &rig.home(), t0 + time::Duration::seconds(4))
        .await
        .unwrap();
    assert_eq!((r.agreed, r.diverged), (2, 0));
    // The agreeing rules went with their watches; the other waits out its
    // grace, then disagrees once.
    assert_eq!(engine.rules(&project()).await.len(), 1);
    let later = t0 + time::Duration::hours(1);
    let r = engine
        .mirror_firstmate_at(&project(), &rig.home(), later)
        .await
        .unwrap();
    assert_eq!(r.diverged, 1);
    assert!(engine.rules(&project()).await.is_empty());
    let d = rig.divergences().await;
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].operation, "trigger.when");
    assert_eq!(d[0].bash["watch"], "when-split");
    assert_eq!(d[0].bash["end"], "fired");
    assert_eq!(d[0].native["end"], "none");
}

// ------------------------------------------------------- real firstmate

fn fm(root: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(root.join("bin/fm-procevent-when.sh"))
        .args(args)
        .env("FM_HOME", home)
        .output()
        .unwrap()
}

/// Arm a watch with firstmate and run its blocking child to the end,
/// capturing the outcome document where the process-event runner would.
fn run_watch(root: &Path, home: &Path, name: &str, options: &[&str], cond: &[&str]) {
    let mut args = vec!["arm", name];
    args.extend_from_slice(options);
    args.push("--condition");
    args.extend_from_slice(cond);
    args.extend_from_slice(&["--action", "true"]);
    let out = fm(root, home, &args);
    assert!(
        out.status.success(),
        "arm {name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sid = format!("when-{name}");
    let out = fm(root, home, &["run", &sid]);
    let inbox = home.join("state/procevent-inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join(format!("{sid}.1.result")), &out.stdout).unwrap();
    assert!(fm(root, home, &["retire", name]).status.success());
}

#[tokio::test]
#[ignore = "needs FIRSTMATE_ROOT, a firstmate checkout"]
async fn real_firstmate_watches_agree() {
    let root = PathBuf::from(std::env::var("FIRSTMATE_ROOT").expect("FIRSTMATE_ROOT"));
    let rig = Rig::new();
    let home = rig.home();
    let engine = rig.engine().await;
    let t0 = OffsetDateTime::now_utc();
    let fast = [
        "--interval",
        "1",
        "--stable",
        "2",
        "--condition-timeout",
        "5",
    ];

    // Arm all three first, so the native side mirrors them while armed.
    let arm_only = |name: &str, extra: &[&str], cond: &[&str]| {
        let mut args = vec!["arm", name];
        args.extend_from_slice(&fast);
        args.extend_from_slice(extra);
        args.push("--condition");
        args.extend_from_slice(cond);
        args.extend_from_slice(&["--action", "true"]);
        let out = fm(&root, &home, &args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    arm_only("holds", &[], &["true"]);
    arm_only("errors", &["--error-budget", "1"], &["sh", "-c", "exit 3"]);
    arm_only("never", &["--deadline", "2"], &["false"]);
    let r = engine
        .mirror_firstmate_at(&project(), &home, t0)
        .await
        .unwrap();
    assert_eq!(r.watches, 3);

    // Native: poll on its own clock until each rule ends.
    for s in 0..4 {
        engine.tick(t0 + time::Duration::seconds(s)).await.unwrap();
    }
    // Firstmate: run each watch's blocking child to its outcome.
    for name in ["holds", "errors", "never"] {
        let sid = format!("when-{name}");
        let out = fm(&root, &home, &["run", &sid]);
        let inbox = home.join("state/procevent-inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(inbox.join(format!("{sid}.1.result")), &out.stdout).unwrap();
        let out = fm(&root, &home, &["retire", name]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let watches = firstmate::read_watches(&home).unwrap();
    let ends: Vec<_> = watches
        .iter()
        .map(|w| (w.source.clone(), w.outcomes.last().map(|o| o.status)))
        .collect();
    assert_eq!(
        ends,
        [
            ("when-errors".into(), Some(WhenStatus::ConditionError)),
            ("when-holds".into(), Some(WhenStatus::Fired)),
            ("when-never".into(), Some(WhenStatus::NeverTrue)),
        ]
    );
    let r = engine
        .mirror_firstmate_at(&project(), &home, OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!(
        (r.agreed, r.diverged),
        (3, 0),
        "{:?}",
        rig.divergences().await
    );
    assert!(engine.rules(&project()).await.is_empty());

    // A watch armed and run whole, after the mirror stopped looking, is
    // never compared: there is no rule to compare it with.
    run_watch(&root, &home, "unseen", &fast, &["true"]);
    let r = engine
        .mirror_firstmate_at(&project(), &home, OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!((r.watches, r.agreed, r.diverged), (0, 0, 0));
}
