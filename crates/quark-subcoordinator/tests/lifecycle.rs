//! Sub-coordinators against real tmux, real git clones and the SQLite event
//! log, with a shell script as the coordinator agent: on this machine and
//! on an "SSH host" reached through a stand-in `ssh`.

mod common;

use std::collections::BTreeMap;

use common::{eventually, git, have_tmux, Env};
use quark_core::host::Health;
use quark_core::worker::WorkerMessage;
use quark_core::{CoreError, ProjectId};
use quark_runtime::RuntimeSpec;
use quark_subcoordinator::{Cause, HandoffItem, Profile, SubCoordinators};

fn reported(s: &SubCoordinators, id: &ProjectId, needle: &str) -> bool {
    s.get(id)
        .unwrap()
        .reports
        .iter()
        .any(|r| r.line.contains(needle))
}

#[tokio::test]
async fn local_sub_coordinator_end_to_end() {
    if !have_tmux() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let env = Env::new("local");
    let subs = env.open().await;
    let id = ProjectId::from("web");

    subs.register(env.registration("web", RuntimeSpec::Local))
        .await
        .unwrap();
    // Nothing runs before the home exists.
    assert!(matches!(subs.launch(&id).await, Err(CoreError::Refused(_))));

    subs.seed(&id).await.unwrap();
    let home = env.home("web");
    assert_eq!(
        git(&home.join("projects/app"), &["remote", "get-url", "origin"]).trim(),
        env.origin.display().to_string()
    );
    assert!(std::fs::read_to_string(home.join("CHARTER.md"))
        .unwrap()
        .contains("Scope: web work"));
    // Seeding again is a no-op.
    subs.seed(&id).await.unwrap();

    let generation = subs.launch(&id).await.unwrap();
    assert_eq!(
        subs.launch(&id).await.unwrap(),
        generation,
        "already running"
    );
    eventually("the first report", async || {
        subs.tick().await.unwrap();
        reported(&subs, &id, &format!("working: started {generation}"))
    })
    .await;

    // A message: delivered as a file, rung in, taken and acknowledged.
    let hello = subs.send(&id, "hello there").await.unwrap();
    eventually("the acknowledgement", async || {
        subs.tick().await.unwrap();
        subs.get(&id)
            .unwrap()
            .outgoing
            .iter()
            .any(|o| o.message.id == hello && o.acknowledged)
    })
    .await;
    assert!(env.took("web").iter().any(|n| n.contains(&hello)));

    // A question comes back as an open decision; the answer closes it.
    subs.send(&id, "please ask").await.unwrap();
    eventually("the decision", async || {
        subs.tick().await.unwrap();
        subs.get(&id).unwrap().decisions.contains_key("k1")
    })
    .await;
    let r = subs.get(&id).unwrap();
    let ask = r
        .reports
        .iter()
        .find(|r| r.line.contains("needs-decision"))
        .unwrap();
    assert!(matches!(ask.message, Some(WorkerMessage::Ask { ref key, .. }) if key == "k1"));
    assert!(subs.answer(&id, "nope", "x").await.is_err());
    subs.answer(&id, "k1", "left").await.unwrap();
    assert!(subs.get(&id).unwrap().decisions.is_empty());
    eventually("the answer taken", async || {
        subs.tick().await.unwrap();
        reported(&subs, &id, "working: going left")
    })
    .await;

    // A handoff blocks retirement until it is taken.
    let items = [HandoffItem {
        key: "fix-login".into(),
        title: "Fix the login form".into(),
        body: "It drops the password on retry.".into(),
        depends_on: vec![],
    }];
    let handoff = subs.handoff(&id, &items).await.unwrap();
    let inbox_file = home
        .join("inbox")
        .read_dir()
        .unwrap()
        .chain(home.join("inbox/handled").read_dir().unwrap())
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().contains(&handoff))
        .unwrap();
    assert!(inbox_file
        .file_name()
        .to_string_lossy()
        .ends_with(".handoff.json"));
    let taken: Vec<HandoffItem> =
        serde_json::from_slice(&std::fs::read(inbox_file.path()).unwrap()).unwrap();
    assert_eq!(taken, items);
    eventually("the handoff taken", async || {
        subs.tick().await.unwrap();
        subs.get(&id).unwrap().pending_handoffs().next().is_none()
    })
    .await;

    // Inherited configuration is written once and announced.
    let material: BTreeMap<String, Vec<u8>> =
        [("captain.md".to_string(), b"Be concise.\n".to_vec())].into();
    assert!(subs.inherit(&id, &material).await.unwrap());
    assert!(!subs.inherit(&id, &material).await.unwrap(), "unchanged");
    assert_eq!(
        std::fs::read(home.join("inherited/captain.md")).unwrap(),
        b"Be concise.\n"
    );
    eventually("the inherit notice taken", async || {
        subs.tick().await.unwrap();
        subs.get(&id)
            .unwrap()
            .outgoing
            .iter()
            .all(|o| o.acknowledged)
    })
    .await;

    // A restarted engine replays the same view and adopts the session.
    let again = env.open().await;
    let tick = again.tick().await.unwrap();
    assert_eq!((tick.exited, tick.recovered, tick.reports), (0, 0, 0));
    assert_eq!(again.get(&id).unwrap(), subs.get(&id).unwrap());

    again.retire(&id, "done with it", false).await.unwrap();
    let s = again.get(&id).unwrap();
    assert!(!s.is_live());
    assert!(home.join("CHARTER.md").exists(), "the home stays");
    let sessions = String::from_utf8_lossy(&env.tmux(&["list-sessions"]).stdout).into_owned();
    assert!(!sessions.contains("sub-web-"), "{sessions}");
    assert!(matches!(
        again.send(&id, "x").await,
        Err(CoreError::Refused(_))
    ));
}

#[tokio::test]
async fn ended_sessions_are_recovered_then_left_to_a_person() {
    if !have_tmux() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let env = Env::new("recover");
    let mut config = env.config();
    config.max_recoveries = 1;
    let subs = SubCoordinators::open(env.log(), env.manifests.clone(), env.hosts(), config)
        .await
        .unwrap();
    let id = ProjectId::from("api");
    subs.register(env.registration("api", RuntimeSpec::Local))
        .await
        .unwrap();
    subs.seed(&id).await.unwrap();
    let first = subs.launch(&id).await.unwrap();

    subs.send(&id, "crash now").await.unwrap();
    eventually("a recovery", async || {
        subs.tick().await.unwrap();
        subs.get(&id)
            .unwrap()
            .current
            .is_some_and(|g| g.id != first && g.cause == Cause::Recover)
    })
    .await;
    let second = subs.get(&id).unwrap().current.unwrap();
    eventually("the new agent reporting", async || {
        subs.tick().await.unwrap();
        reported(&subs, &id, &format!("started {}", second.id))
    })
    .await;
    let brief = std::fs::read_to_string(env.home("api").join("BRIEF.md")).unwrap();
    assert!(brief.starts_with("Your previous session ended unexpectedly (exit code 3)."));

    // Past the limit the agent stays stopped until a person relaunches it.
    subs.send(&id, "crash again").await.unwrap();
    eventually("the second exit", async || {
        subs.tick().await.unwrap();
        subs.get(&id).unwrap().current.unwrap().exited.is_some()
    })
    .await;
    subs.tick().await.unwrap();
    assert_eq!(subs.get(&id).unwrap().current.unwrap().id, second.id);

    let third = subs
        .relaunch(
            &id,
            Some(Profile {
                harness: "fake".into(),
                model: None,
                effort: Some("high".into()),
            }),
            "Fresh start.",
        )
        .await
        .unwrap();
    let g = subs.get(&id).unwrap().current.unwrap();
    assert_eq!((g.id.as_str(), g.cause), (third.as_str(), Cause::Relaunch));
    assert_eq!(g.profile.effort.as_deref(), Some("high"));
    assert_eq!(subs.get(&id).unwrap().recoveries, 0);
    subs.stop(&id).await.unwrap();
    assert!(!subs.get(&id).unwrap().is_running());
}

#[tokio::test]
async fn ssh_sub_coordinator_survives_an_unreachable_host() {
    if !have_tmux() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let env = Env::new("ssh");
    let subs = env.open().await;
    let id = ProjectId::from("ios");
    subs.register(env.registration("ios", env.ssh_spec()))
        .await
        .unwrap();
    subs.seed(&id).await.unwrap();
    assert!(env.home("ios").join("projects/app/README").exists());
    let generation = subs.launch(&id).await.unwrap();
    eventually("the first report over ssh", async || {
        subs.tick().await.unwrap();
        reported(&subs, &id, "working: started")
    })
    .await;
    assert_eq!(subs.get(&id).unwrap().health, Some(Health::Healthy));

    // The host drops off the network: recorded, nothing replaced.
    env.set_down(true);
    let tick = subs.tick().await.unwrap();
    assert_eq!((tick.unreachable, tick.recovered), (1, 0));
    assert!(matches!(
        subs.get(&id).unwrap().health,
        Some(Health::Unreachable { .. })
    ));
    let waiting = subs.send(&id, "while you were away").await.unwrap();
    assert!(subs
        .get(&id)
        .unwrap()
        .undelivered()
        .any(|o| o.message.id == waiting));
    assert!(subs.launch(&id).await.is_err(), "launch needs the host");

    // Back again: the same agent gets the message.
    env.set_down(false);
    eventually("delivery after reconnecting", async || {
        subs.tick().await.unwrap();
        subs.get(&id)
            .unwrap()
            .outgoing
            .iter()
            .any(|o| o.message.id == waiting && o.acknowledged)
    })
    .await;
    let s = subs.get(&id).unwrap();
    assert_eq!(s.current.unwrap().id, generation);
    assert_eq!(s.health, Some(Health::Healthy));

    // Retiring needs the host too, unless forced.
    env.set_down(true);
    assert!(matches!(
        subs.retire(&id, "moving", false).await,
        Err(CoreError::Refused(_))
    ));
    env.set_down(false);
    subs.retire(&id, "moving", false).await.unwrap();
}

#[tokio::test]
async fn refusals() {
    let env = Env::new("refuse");
    let subs = env.open().await;

    let mut r = env.registration("web", RuntimeSpec::Local);
    r.profile.harness = "workeronly".into();
    assert!(matches!(subs.register(r).await, Err(CoreError::Refused(_))));
    let mut r = env.registration("web", RuntimeSpec::Local);
    r.profile.effort = Some("max".into());
    assert!(matches!(subs.register(r).await, Err(CoreError::Invalid(_))));

    subs.register(env.registration("web", RuntimeSpec::Local))
        .await
        .unwrap();
    assert!(subs
        .register(env.registration("web", RuntimeSpec::Local))
        .await
        .is_err());
    let mut nested = env.registration("web2", RuntimeSpec::Local);
    nested.placement.home = env.home("web").join("inner");
    assert!(matches!(
        subs.register(nested).await,
        Err(CoreError::Refused(_))
    ));

    // Someone else's directory is never taken over.
    let foreign = env.registration("other", RuntimeSpec::Local);
    std::fs::create_dir_all(&foreign.placement.home).unwrap();
    std::fs::write(foreign.placement.home.join("notes.txt"), "mine").unwrap();
    let oid = foreign.id.clone();
    subs.register(foreign).await.unwrap();
    if have_tmux() {
        let err = subs.seed(&oid).await.unwrap_err();
        assert!(
            err.to_string().contains("not a sub-coordinator home"),
            "{err}"
        );
    }

    // A host without the harness is not ready, and says what to do.
    let mut missing = env.registration("bare", RuntimeSpec::Local);
    missing.profile = Profile {
        harness: "missing".into(),
        model: None,
        effort: None,
    };
    let bid = missing.id.clone();
    subs.register(missing).await.unwrap();
    let ready = subs.doctor(&bid).await.unwrap();
    assert!(!ready.ready());
    assert!(
        ready.summary().contains("install the test agent"),
        "{}",
        ready.summary()
    );
    let err = subs.seed(&bid).await.unwrap_err();
    assert!(err.to_string().contains("not ready"), "{err}");
    assert!(!env.home("bare").exists());
}

#[tokio::test]
async fn shadow_compares_with_the_bash_registry() {
    use quark_subcoordinator::shadow::{compare, parse_registry};
    let env = Env::new("shadow");
    let subs = env.open().await;
    subs.register(env.registration("web", RuntimeSpec::Local))
        .await
        .unwrap();
    subs.register(env.registration("ios", env.ssh_spec()))
        .await
        .unwrap();
    let bash = format!(
        "- web - web domain (home: {}; scope: web work; projects: app; added 2026-10-07)\n\
         - ios - ios domain (host: build-box; root: /r/fm; home: {}; scope: ios work; projects: app; added 2026-10-07)\n",
        env.home("web").display(),
        env.home("ios").display()
    );
    assert!(compare(&subs.registry().all(), &parse_registry(&bash)).is_empty());

    let drifted = bash.replace("scope: web work", "scope: frontend work");
    let d = compare(&subs.registry().all(), &parse_registry(&drifted));
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].bash["scope"], "frontend work");
    assert_eq!(d[0].native["scope"], "web work");

    let only_web: String = bash.lines().take(1).collect();
    let d = compare(&subs.registry().all(), &parse_registry(&only_web));
    assert_eq!(d.len(), 1);
    assert!(d[0].bash.is_null());
    assert_eq!(d[0].native["id"], "ios");
}
