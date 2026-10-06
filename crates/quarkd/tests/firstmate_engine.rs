//! FirstmateEngine against a fake engine that prints a snapshot captured
//! from the real firstmate engine.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use quark_engine::runner::MemoryCallLog;
use quark_systems::CreateProject;
use quark_systems::{AgentConfig, DeliveryPolicy, DispatchStatus, TaskKind, TaskState};
use quarkd::config::StoreCallLog;
use quarkd::engine::firstmate::FirstmateEngine;
use quarkd::engine::{
    EngineAdapter, EngineError, SourceRepo, TaskControl, WorkspacePlan, WorkspaceRef,
};
use quarkd::store::Store;

struct Fake {
    home: PathBuf,
    engine_root: PathBuf,
}

fn fake_engine(dir: &Path) -> Fake {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../quark-engine/tests/fixtures/fleet-snapshot.v1.json");
    let bin = dir.join("engine/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(dir.join("home/state")).unwrap();
    let script = bin.join("fm-fleet-snapshot.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n[ \"$1\" = --json ] || exit 2\ncat '{}'\n",
            fixture.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    Fake {
        home: dir.join("home"),
        engine_root: dir.join("engine"),
    }
}

fn engine(dir: &Path) -> (FirstmateEngine, WorkspaceRef) {
    let ws = fake_engine(dir);
    (
        FirstmateEngine::new(&ws.engine_root, Arc::new(MemoryCallLog::default())),
        WorkspaceRef {
            project_id: "p1".into(),
            root: ws.home.clone(),
        },
    )
}

#[tokio::test]
async fn snapshot_maps_to_neutral_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let (e, ws) = engine(dir.path());
    assert_eq!(e.name(), "firstmate");
    let snap = e.snapshot(&ws).await.unwrap();
    let got: Vec<_> = snap
        .tasks
        .iter()
        .map(|t| (t.id.as_str(), t.kind, t.state))
        .collect();
    assert_eq!(
        got,
        [
            ("external-wait", Some(TaskKind::Ship), TaskState::Paused),
            ("scout-x", Some(TaskKind::Scout), TaskState::Done),
            ("ship-task", Some(TaskKind::Ship), TaskState::Running),
            ("live-gate", Some(TaskKind::Ship), TaskState::Queued),
            ("dead-gate", Some(TaskKind::Scout), TaskState::Queued),
        ],
        "secondmates and captain-kind rows are not tasks"
    );
    let ship = &snap.tasks[2];
    assert!(ship.worktree.is_some(), "live tasks carry their worktree");
    assert_eq!(ship.title, "Ship the thing");
    assert_eq!(ship.harness.as_deref(), Some("claude"));
    assert_eq!(
        ship.pull_request_url.as_deref(),
        Some("https://github.com/kunchenguid/firstmate/pull/9")
    );
    assert_eq!(ship.terminal.as_deref(), Some("firstmate:fm-ship-task"));
}

#[tokio::test]
async fn coordinators_are_secondmate_windows() {
    let dir = tempfile::tempdir().unwrap();
    let (e, ws) = engine(dir.path());
    let got = e.coordinator_terminals(&ws.root).await.unwrap();
    assert_eq!(
        got.into_iter().collect::<Vec<_>>(),
        [("mate".to_string(), "firstmate:fm-mate".to_string())]
    );
    let none = e
        .coordinator_terminals(&dir.path().join("missing"))
        .await
        .unwrap();
    assert!(
        none.is_empty(),
        "no command center yet means no coordinators"
    );
}

#[tokio::test]
async fn holds_are_live_holds_and_open_decisions() {
    let dir = tempfile::tempdir().unwrap();
    let (e, ws) = engine(dir.path());
    let holds = e.holds(&ws).await.unwrap();
    let got: Vec<_> = holds
        .iter()
        .map(|h| (h.id.as_str(), h.question.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("pick-db", "Choose the projection store: SQLite or DuckDB"),
            ("mate:race", "pick subscribe order"),
        ]
    );
}

#[tokio::test]
async fn status_tail_resumes_from_offset() {
    let dir = tempfile::tempdir().unwrap();
    let (e, ws) = engine(dir.path());
    fs::write(
        ws.root.join("state/t.status"),
        "working: a\nneeds-decision [key=api]: pick one\n",
    )
    .unwrap();
    let first = e.status_tail(&ws, "t", 0).await.unwrap();
    let got: Vec<_> = first
        .entries
        .iter()
        .map(|l| (l.kind.as_str(), l.decision_key.as_deref(), l.note.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("working", Some("default"), "a"),
            ("needs-decision", Some("api"), "pick one")
        ]
    );
    assert_eq!(first.entries[1].raw, "needs-decision [key=api]: pick one");
    let again = e.status_tail(&ws, "t", first.next_offset).await.unwrap();
    assert!(again.entries.is_empty());
    assert_eq!(again.next_offset, first.next_offset);
    assert!(matches!(
        e.status_tail(&ws, "../x", 0).await,
        Err(EngineError::TaskNotFound(_))
    ));
}

#[tokio::test]
async fn spawns_and_dispatch_resolution_come_from_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let (e, ws) = engine(dir.path());
    let state = ws.root.join("state");
    fs::write(
        state.join("ship-task.meta"),
        "harness=codex\nkind=ship\nmodel=gpt-5.6-luna\neffort=default\n\
         spawn_gen=s1790000000.7.1\nproject=/h/projects/quark\n",
    )
    .unwrap();
    fs::write(
        state.join("mate.meta"),
        "harness=claude\nkind=secondmate\nspawn_gen=s1.1.1\n",
    )
    .unwrap();
    let spawn = e.spawn(&ws, "ship-task").await.unwrap().unwrap();
    assert_eq!(spawn.harness, "codex");
    assert_eq!(spawn.model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(spawn.effort, None);
    assert_eq!(spawn.spawned_at, Some(1_790_000_000));
    assert_eq!(spawn.project.as_deref(), Some("quark"));
    assert!(
        e.spawn(&ws, "mate").await.unwrap().is_none(),
        "secondmates are not workers"
    );
    assert!(e.spawn(&ws, "scout-x").await.unwrap().is_none());

    // No brief: the resolution is not run.
    assert!(e
        .resolve_dispatch(&ws, "ship-task", Some("quark"))
        .await
        .unwrap()
        .is_none());

    let script = ws_engine_bin(dir.path()).join("fm-dispatch-resolve.sh");
    fs::write(
        &script,
        "#!/bin/sh\n[ \"$3\" = quark ] || exit 2\ncat <<'OUT'\ndispatch-resolve:\n  status: clear\n\
         model: jev-1.13.0   latency_ms: 200   tokens: 1/1\n\
         rule: rule_1 (A trivial edit.)   confidence: 0.88\n\
         candidate: codex:gpt-5.6-luna  provider=codex  scope=all_models  remaining=55%  spendPriority=0.2  runway=through_reset  -> eligible\n\
         candidate: claude:opus  provider=claude  -> not eligible: runway exhausted_now at all_models\n\
         profile: --harness 'codex' --model 'gpt-5.6-luna'\nOUT\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(ws.root.join("data/ship-task")).unwrap();
    fs::write(ws.root.join("data/ship-task/brief.md"), "rename a field\n").unwrap();

    let r = e
        .resolve_dispatch(&ws, "ship-task", Some("quark"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.status, DispatchStatus::Clear);
    assert_eq!(r.rule.as_ref().unwrap().id, "rule_1");
    assert_eq!(
        r.rule.as_ref().unwrap().when.as_deref(),
        Some("A trivial edit.")
    );
    assert!(r.classifier_consulted);
    assert_eq!(r.classifier_model.as_deref(), Some("jev-1.13.0"));
    assert_eq!(r.confidence, Some(0.88));
    assert_eq!(r.candidates.len(), 2);
    assert!(r.candidates[0].passed);
    assert!(!r.candidates[1].passed);
    assert_eq!(r.candidates[1].reason, "runway exhausted_now at all_models");
    let p = r.profile.unwrap();
    assert_eq!(
        (p.harness.as_str(), p.model.as_deref()),
        ("codex", Some("gpt-5.6-luna"))
    );
    assert!(r.output.unwrap().starts_with("dispatch-resolve:"));

    // A configuration error (exit 2) is an engine failure, not a result.
    assert!(e
        .resolve_dispatch(&ws, "ship-task", Some("other"))
        .await
        .is_err());
}

#[tokio::test]
async fn a_description_is_resolved_as_a_brief_without_a_task() {
    let dir = tempfile::tempdir().unwrap();
    let (e, ws) = engine(dir.path());
    let script = ws_engine_bin(dir.path()).join("fm-dispatch-resolve.sh");
    // Resolves only a readable brief holding the description, passed alone.
    fs::write(
        &script,
        "#!/bin/sh\n[ $# -eq 1 ] || exit 2\necho \"$1\" > \"$FM_HOME/brief.path\"\n\
         grep -q 'no rule for this' \"$1\" && exit 0\n\
         grep -q 'rename a field' \"$1\" || exit 2\ncat <<'OUT'\ndispatch-resolve:\n  status: ambiguous\n\
         model: jev-1.13.0   latency_ms: 200   tokens: 1/1\n\
         rule: rule_1 (A trivial edit.)   confidence: 0.4\n\
         reason: confidence 0.4 below floor 0.6\n\
         candidate: claude:sonnet  provider=claude  scope=all_models  remaining=79%  -> eligible\nOUT\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let r = e
        .resolve_description(&ws, "rename a field")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.status, DispatchStatus::Ambiguous);
    assert_eq!(r.rule.as_ref().unwrap().id, "rule_1");
    assert_eq!(r.confidence, Some(0.4));
    assert_eq!(r.candidates.len(), 1);
    assert!(r.profile.is_none());
    let brief = PathBuf::from(
        fs::read_to_string(ws.root.join("brief.path"))
            .unwrap()
            .trim(),
    );
    assert!(brief.is_absolute());
    assert!(!brief.exists(), "the temporary brief is removed");
    assert!(
        !brief.starts_with(&ws.root),
        "nothing is written in the workspace"
    );

    // No classifier key: the script prints nothing, which reads as off.
    let off = e
        .resolve_description(&ws, "no rule for this")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(off.status, DispatchStatus::Off);
    assert!(!off.classifier_consulted);

    // A configuration error (exit 2) is an engine failure, not a result.
    assert!(e.resolve_description(&ws, "something else").await.is_err());
}

fn ws_engine_bin(dir: &Path) -> PathBuf {
    dir.join("engine/bin")
}

#[tokio::test]
async fn missing_workspace_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (e, mut ws) = engine(dir.path());
    ws.root = dir.path().join("nope");
    assert!(matches!(
        e.snapshot(&ws).await,
        Err(EngineError::WorkspaceNotFound(_))
    ));
}

/// Fake write scripts that append their argv, one argument per line, to
/// `<home>/calls.log`. `fm-control.sh` fails for task `stuck`.
fn add_write_scripts(engine_root: &Path) {
    for (name, extra) in [
        ("fm-send.sh", ""),
        (
            "fm-control.sh",
            "[ \"$1\" = stuck ] && { echo 'agent did not stop' >&2; exit 1; }\n",
        ),
    ] {
        let p = engine_root.join("bin").join(name);
        fs::write(
            &p,
            format!(
                "#!/bin/sh\n{extra}{{ echo '--- {name}'; for a in \"$@\"; do echo \"$a\"; done; }} >> \"$FM_HOME/calls.log\"\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[tokio::test]
async fn writes_run_allowlisted_scripts_and_are_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_engine(dir.path());
    add_write_scripts(&fake.engine_root);
    let store = Arc::new(Store::open_in_memory().unwrap());
    let project = store
        .create_project(CreateProject {
            name: "Quark".into(),
            goal: None,
            workspace_path: Some(fake.home.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .unwrap();
    let e = FirstmateEngine::new(
        &fake.engine_root,
        Arc::new(StoreCallLog {
            store: store.clone(),
        }),
    );
    let ws = WorkspaceRef {
        project_id: project.id.clone(),
        root: fake.home.clone(),
    };

    e.send_message(&ws, "ship-task", "line one\n- line two")
        .await
        .unwrap();
    e.control(&ws, "ship-task", &TaskControl::Cancel)
        .await
        .unwrap();
    e.control(
        &ws,
        "ship-task",
        &TaskControl::Relaunch {
            harness: Some("codex".into()),
            model: None,
            effort: Some("high".into()),
            note: "--resume from the red test".into(),
            account_env: Vec::new(),
        },
    )
    .await
    .unwrap();

    let log = fs::read_to_string(fake.home.join("calls.log")).unwrap();
    assert_eq!(
        log,
        "--- fm-send.sh\nship-task\nline one\n- line two\n\
         --- fm-control.sh\nship-task\nexit\n\
         --- fm-control.sh\nship-task\nrelaunch\n--harness=codex\n--effort=high\n\
         --note=--resume from the red test\n"
    );

    // Refused before anything runs: no script call, no record.
    let err = e.send_message(&ws, "ship-task", "/quit").await.unwrap_err();
    assert!(matches!(err, EngineError::Invalid(_)), "{err:?}");
    let err = e
        .control(&ws, "--help", &TaskControl::Cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::TaskNotFound(_)), "{err:?}");

    let err = e
        .control(&ws, "stuck", &TaskControl::Cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::Command(ref m) if m.contains("agent did not stop")));

    let calls = store.recent_script_calls(10).unwrap();
    let got: Vec<_> = calls
        .iter()
        .rev()
        .map(|c| (c.script.as_str(), c.args[1].as_str(), c.ok, c.exit_code))
        .collect();
    assert_eq!(
        got,
        [
            ("fm-send.sh", "line one\n- line two", true, Some(0)),
            ("fm-control.sh", "exit", true, Some(0)),
            ("fm-control.sh", "relaunch", true, Some(0)),
            ("fm-control.sh", "exit", false, Some(1)),
        ]
    );
    assert!(calls.iter().all(|c| c.kind == "write"));
    assert!(calls
        .iter()
        .all(|c| c.project_id.as_deref() == Some(project.id.as_str())));
    assert_eq!(calls[0].detail.as_deref(), Some("agent did not stop\n"));
}

#[tokio::test]
async fn answers_go_to_the_inbox_or_the_hold_record() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_engine(dir.path());
    add_write_scripts(&fake.engine_root);
    // Logs its argv and the decision file's content, which must still exist.
    let hold = fake.engine_root.join("bin/fm-captain-hold.sh");
    fs::write(
        &hold,
        "#!/bin/sh
{ echo '--- fm-captain-hold.sh'; for a in \"$@\"; do echo \"$a\"; done; \
         echo '>>>'; cat \"$4\"; echo; } >> \"$FM_HOME/calls.log\"\necho \"answered: $2\"\n",
    )
    .unwrap();
    fs::set_permissions(&hold, fs::Permissions::from_mode(0o755)).unwrap();
    let e = FirstmateEngine::new(&fake.engine_root, Arc::new(MemoryCallLog::default()));
    let ws = WorkspaceRef {
        project_id: "p1".into(),
        root: fake.home.clone(),
    };

    e.answer(&ws, "mate:race", "subscribe first", "Matt S")
        .await
        .unwrap();
    e.answer(&ws, "pick-db", "SQLite", "Matt S").await.unwrap();

    let log = fs::read_to_string(fake.home.join("calls.log")).unwrap();
    let mut lines = log.lines();
    let decision_file = log
        .lines()
        .skip_while(|l| *l != "--decision-file")
        .nth(1)
        .unwrap()
        .to_string();
    let want = format!(
        "--- fm-send.sh\nmate\n--resolve-key=race\n--answered-by=Matt S\nsubscribe first\n\
         --- fm-captain-hold.sh\nanswer\npick-db\n--decision-file\n{decision_file}\n\
         --answered-by\nMatt S\n>>>\nSQLite"
    );
    assert_eq!(lines.by_ref().collect::<Vec<_>>().join("\n"), want);
    assert!(
        !Path::new(&decision_file).exists(),
        "the answer file is removed once recorded"
    );

    // Only live captain holds can be answered; a deferred one is not open.
    let err = e.answer(&ws, "later-call", "x", "Matt").await.unwrap_err();
    assert!(matches!(err, EngineError::TaskNotFound(_)), "{err:?}");
    let err = e.answer(&ws, "mate:race", "x", "a\nb").await.unwrap_err();
    assert!(matches!(err, EngineError::Invalid(_)), "{err:?}");
}

/// Fake provisioning scripts: each logs its argv and the charter env, then
/// prints the result line the real script prints.
fn add_provision_scripts(engine_root: &Path) {
    for (name, result) in [
        (
            "fm-project-add.sh",
            "echo \"project=$1 path=$FM_HOME/projects/$1 mode=$4 yolo=off result=added\"",
        ),
        ("fm-home-seed.sh", "mkdir -p \"$2\"; echo \"home=$2\""),
        (
            "fm-spawn.sh",
            "echo \"spawned $1 harness=$4 kind=secondmate window=fm:$1 worktree=$2\"",
        ),
    ] {
        let p = engine_root.join("bin").join(name);
        fs::write(
            &p,
            format!(
                "#!/bin/sh\n{{ echo '--- {name}'; for a in \"$@\"; do echo \"$a\"; done; \
                 [ -n \"$FM_SECONDMATE_CHARTER\" ] && echo \"charter=$FM_SECONDMATE_CHARTER\"; \
                 [ -n \"$FM_SECONDMATE_SCOPE\" ] && echo \"scope=$FM_SECONDMATE_SCOPE\"; }} \
                 >> \"$FM_HOME/calls.log\"\n{result}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn plan(root: PathBuf) -> WorkspacePlan {
    WorkspacePlan {
        user_memory: None,
        project_id: "prj_1".into(),
        name: "Quark".into(),
        goal: Some("Ship J2".into()),
        sources: vec![
            SourceRepo {
                name: "quark".into(),
                url: "https://github.com/quark-systems/quark.git".into(),
            },
            SourceRepo {
                name: "engine".into(),
                url: "git@github.com:quark-systems/firstmate.git".into(),
            },
        ],
        root,
    }
}

#[tokio::test]
async fn provisioning_runs_project_add_seed_and_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_engine(dir.path());
    add_provision_scripts(&fake.engine_root);
    let e = FirstmateEngine::new(&fake.engine_root, Arc::new(MemoryCallLog::default()));
    let command = fake.home.clone();
    let plan = plan(dir.path().join("workspaces/prj_1"));

    for s in &plan.sources {
        e.add_source(&command, s, DeliveryPolicy::Gated)
            .await
            .unwrap();
    }
    let root = e.seed_workspace(&command, &plan).await.unwrap();
    assert_eq!(root, plan.root);
    let ws = WorkspaceRef {
        project_id: "prj_1".into(),
        root,
    };
    e.start_coordinator(
        &command,
        &ws,
        &AgentConfig {
            harness: "claude-code".into(),
            model: Some("claude-sonnet-5".into()),
            effort: Some("high".into()),
            pool: None,
        },
        &[],
    )
    .await
    .unwrap();

    let root = plan.root.to_str().unwrap();
    let log = fs::read_to_string(command.join("calls.log")).unwrap();
    assert_eq!(
        log,
        format!(
            "--- fm-project-add.sh\nquark\nhttps://github.com/quark-systems/quark.git\n--mode\nno-mistakes\n\
             --desc\nhttps://github.com/quark-systems/quark.git (added by Quark)\n\
             --- fm-project-add.sh\nengine\ngit@github.com:quark-systems/firstmate.git\n--mode\nno-mistakes\n\
             --desc\ngit@github.com:quark-systems/firstmate.git (added by Quark)\n\
             --- fm-home-seed.sh\nprj_1\n{root}\nquark\nengine\n\
             charter=Coordinate the Quark Project \"Quark\" across quark, engine. Its goal: Ship J2 \
             The Project repo checked out at project/ holds its instructions.md and memory/; \
             read instructions.md and every entry in memory/ before planning work, and reread memory/ \
             when told a new entry landed. Have workers report what a task taught them that is worth \
             keeping as `learned: <text>` status lines before done:, and add your own for a finished \
             task as `learned [source=coordinator]: <text>` in its status log; each becomes a memory \
             proposal for review.\n\
             scope=All work for the Quark Project \"Quark\" (prj_1) in quark, engine.\n\
             --- fm-spawn.sh\nprj_1\n{root}\n--harness\nclaude\n--model\nclaude-sonnet-5\n--effort\nhigh\n--secondmate\n"
        )
    );

    // A missing command-center workspace is reported, not created.
    let err = e
        .add_source(
            &dir.path().join("nope"),
            &plan.sources[0],
            DeliveryPolicy::Direct,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::WorkspaceNotFound(_)), "{err:?}");
}

/// Provisions against a real engine checkout with the fork's
/// `fm-project-add.sh`: clones a local repo, seeds a workspace and checks the
/// registry. The coordinator launch needs tmux and a harness, so it is left
/// out. Set FIRSTMATE_ROOT, then `cargo test -- --ignored`.
#[tokio::test]
#[ignore]
async fn live_engine_seeds_a_workspace() {
    let root = PathBuf::from(std::env::var("FIRSTMATE_ROOT").expect("FIRSTMATE_ROOT"));
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let src = dir.path().join("src");
    git(&["init", "-q", "-b", "main", src.to_str().unwrap()]);
    fs::write(src.join("README.md"), "hi\n").unwrap();
    git(&["-C", src.to_str().unwrap(), "add", "."]);
    git(&[
        "-C",
        src.to_str().unwrap(),
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "init",
    ]);
    let bare = dir.path().join("app.git");
    git(&[
        "clone",
        "-q",
        "--bare",
        src.to_str().unwrap(),
        bare.to_str().unwrap(),
    ]);

    let command = dir.path().join("workspaces/command");
    fs::create_dir_all(&command).unwrap();
    let e = FirstmateEngine::new(&root, Arc::new(MemoryCallLog::default()));
    let source = SourceRepo {
        name: "app".into(),
        url: bare.to_str().unwrap().into(),
    };
    e.add_source(&command, &source, DeliveryPolicy::Direct)
        .await
        .unwrap();
    // Idempotent.
    e.add_source(&command, &source, DeliveryPolicy::Direct)
        .await
        .unwrap();
    let mut plan = plan(dir.path().join("workspaces/prj_1"));
    plan.sources = vec![source];
    let home = e.seed_workspace(&command, &plan).await.unwrap();
    assert!(home.join("projects/app/README.md").is_file());
    assert!(home.join("data/charter.md").is_file());
    let registry = fs::read_to_string(command.join("data/secondmates.md")).unwrap();
    assert!(registry.starts_with("- prj_1 - "), "{registry}");
}

#[tokio::test]
async fn engine_scripts_run_on_the_daemons_tmux_server() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_engine(dir.path());
    let spawn = fake.engine_root.join("bin/fm-spawn.sh");
    fs::write(
        &spawn,
        "#!/bin/sh\necho \"TMUX=$TMUX FM_HOME=$FM_HOME CLAUDE_CONFIG_DIR=$CLAUDE_CONFIG_DIR\" > \"$FM_HOME/env.log\"\n\
         echo \"spawned $1 harness=$4 kind=secondmate window=fm:$1 worktree=$2\"\n",
    )
    .unwrap();
    fs::set_permissions(&spawn, fs::Permissions::from_mode(0o755)).unwrap();
    let e = FirstmateEngine::new(&fake.engine_root, Arc::new(MemoryCallLog::default()))
        .with_tmux(Some("/run/quark/tmux/quark,0,0".into()));
    let root = dir.path().join("workspaces/prj_1");
    fs::create_dir_all(&root).unwrap();
    let ws = WorkspaceRef {
        project_id: "prj_1".into(),
        root,
    };
    e.start_coordinator(
        &fake.home,
        &ws,
        &AgentConfig {
            harness: "claude-code".into(),
            model: None,
            effort: None,
            pool: Some("max".into()),
        },
        &[("CLAUDE_CONFIG_DIR".into(), "/accounts/claude-work".into())],
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(fake.home.join("env.log")).unwrap(),
        format!(
            "TMUX=/run/quark/tmux/quark,0,0 FM_HOME={} CLAUDE_CONFIG_DIR=/accounts/claude-work\n",
            fake.home.display()
        )
    );

    // A relaunch carries the account it moves the worker to: the engine's
    // own `fm-control.sh <task> relaunch`, with the variable in its
    // environment.
    let control = fake.engine_root.join("bin/fm-control.sh");
    fs::write(
        &control,
        "#!/bin/sh\necho \"$1 $2 CODEX_HOME=$CODEX_HOME CLAUDE_CONFIG_DIR=$CLAUDE_CONFIG_DIR\" > \"$FM_HOME/env.log\"\n",
    )
    .unwrap();
    fs::set_permissions(&control, fs::Permissions::from_mode(0o755)).unwrap();
    e.control(
        &WorkspaceRef {
            project_id: "prj_1".into(),
            root: fake.home.clone(),
        },
        "ship-task",
        &TaskControl::Relaunch {
            harness: None,
            model: None,
            effort: None,
            note: "moved after a rate limit".into(),
            account_env: vec![("CODEX_HOME".into(), "/accounts/codex-work".into())],
        },
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(fake.home.join("env.log")).unwrap(),
        "ship-task relaunch CODEX_HOME=/accounts/codex-work CLAUDE_CONFIG_DIR=\n"
    );

    // Only an account variable the engine forwards, set to an absolute
    // path, reaches a script.
    for env in [
        ("PI_CODING_AGENT_DIR", "/accounts/pi-work"),
        ("CLAUDE_CONFIG_DIR", "relative/dir"),
        ("PATH", "/tmp"),
    ] {
        let err = e
            .start_coordinator(
                &fake.home,
                &ws,
                &AgentConfig {
                    harness: "claude-code".into(),
                    model: None,
                    effort: None,
                    pool: None,
                },
                &[(env.0.into(), env.1.into())],
            )
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::Invalid(_)), "{env:?}: {err}");
    }
}

#[test]
fn only_task_files_trigger_refresh() {
    use quarkd::engine::firstmate::is_task_file;
    for yes in ["state/t.status", "state/t.meta", "data/backlog.md"] {
        assert!(is_task_file(Path::new(yes)), "{yes}");
    }
    for no in [
        "state/.last-watcher-beat",
        "state/.t.meta.spawn.123",
        "state/t.turn-ended",
        "state/.wake-queue",
    ] {
        assert!(!is_task_file(Path::new(no)), "{no}");
    }
}

#[tokio::test]
async fn crew_dispatch_config_goes_through_config_set() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_engine(dir.path());
    // Mirrors fm-crew-dispatch.sh config-set: input only from the
    // environment, exit 1 on an invalid config with the old file untouched.
    let script = fake.engine_root.join("bin/fm-crew-dispatch.sh");
    fs::write(
        &script,
        "#!/bin/sh\n[ \"$#\" = 1 ] && [ \"$1\" = config-set ] || { echo 'error: usage' >&2; exit 2; }\n\
         case \"$FM_CREW_DISPATCH_CONFIG_JSON\" in *nope*) echo 'error: unverified harness: nope' >&2; exit 1;; esac\n\
         mkdir -p \"$FM_HOME/config\"\n\
         printf '%s' \"$FM_CREW_DISPATCH_CONFIG_JSON\" > \"$FM_HOME/config/crew-dispatch.json\"\n\
         echo \"crew dispatch config written: $FM_HOME/config/crew-dispatch.json\"\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let log = Arc::new(MemoryCallLog::default());
    let e = FirstmateEngine::new(&fake.engine_root, log.clone());
    let ws = WorkspaceRef {
        project_id: "p1".into(),
        root: fake.home.clone(),
    };
    let file = fake.home.join("config/crew-dispatch.json");

    let good = r#"{"rules":[],"default":{"harness":"claude"}}"#;
    e.set_crew_dispatch(&ws, good).await.unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), good);

    let err = e
        .set_crew_dispatch(&ws, r#"{"default":{"harness":"nope"}}"#)
        .await
        .unwrap_err();
    assert!(
        matches!(err, EngineError::Invalid(ref m) if m == "crew dispatch config refused: unverified harness: nope"),
        "{err:?}"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), good);

    // Refused before anything runs.
    let err = e.set_crew_dispatch(&ws, "[]").await.unwrap_err();
    assert!(matches!(err, EngineError::Invalid(_)), "{err:?}");
    let calls = log.calls();
    assert_eq!(calls.len(), 2);
    assert!(calls
        .iter()
        .all(|c| c.script == "fm-crew-dispatch.sh" && c.args == ["config-set"]));
}

#[test]
fn charter_points_at_user_level_memory() {
    let mut plan = plan(PathBuf::from("/w"));
    let (without, _) = quarkd::engine::firstmate::charter(&plan);
    assert!(!without.contains("shared by every Project"), "{without}");
    plan.user_memory = Some(PathBuf::from("/home/me/.quark/memory"));
    let (with, _) = quarkd::engine::firstmate::charter(&plan);
    assert!(
        with.ends_with(
            " Memory shared by every Project is in /home/me/.quark/memory; read every entry \
             there too, and reread it when told a new one landed."
        ),
        "{with}"
    );
}

/// Regression: the charter quarkd builds (goal plus shared memory) outgrew
/// the 600-character line bound and the engine refused every new Project.
#[test]
fn full_charter_passes_engine_validation() {
    let mut plan = plan(PathBuf::from("/w"));
    plan.goal = Some("Build the Quark MVP according to the spec. ".repeat(10));
    plan.user_memory = Some(PathBuf::from("/home/me/.quark/memory"));
    let (charter, scope) = quarkd::engine::firstmate::charter(&plan);
    assert!(charter.chars().count() > quark_engine::write::MAX_LINE_CHARS);
    let op = quark_engine::write::WriteOp::HomeSeed {
        id: "prj_1".into(),
        home: "/w/prj_1".into(),
        projects: vec!["quark".into(), "engine".into()],
        charter,
        scope,
    };
    op.argv().expect("charter accepted");
}
