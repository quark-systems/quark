//! The dispatcher end to end over the in-memory log, with a fake host
//! supervisor that records `Started` the way the real one does.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use quark_core::event::kinds;
use quark_core::fake::{MemoryEventLog, MemoryHosts};
use quark_core::host::{Capacity, Health, Host, HostRegistry, Platform, RuntimeKind};
use quark_core::isolation::IsolationMode;
use quark_core::task::TaskEvent;
use quark_core::telemetry::{HostSample, QuarkDisk};
use quark_core::verify::{Change, MainHealth, Permit, Verdict};
use quark_core::{EventLog, HostId, MergeGuard, NewEvent, ProjectId, Result, TaskId};
use quark_dispatch::classify::{Answer, Classification};
use quark_dispatch::{
    DispatchConfig, DispatchEvent, Dispatcher, FixedConfig, FixedQuota, Given, IdlePool, Parts,
    PoolAccount, Profile, ProviderFamilies, QuotaSnapshot, Request, Settings, Spawner, Stage,
    StaticAccounts,
};
use quark_supervisor::SpawnRequest;
use time::OffsetDateTime;

#[derive(Clone)]
struct FakeHosts {
    log: MemoryEventLog,
    spawned: Arc<Mutex<Vec<(HostId, SpawnRequest)>>>,
}

#[async_trait]
impl Spawner for FakeHosts {
    async fn spawn(&self, host: &HostId, request: SpawnRequest) -> Result<()> {
        let started = NewEvent::typed(
            host.clone(),
            request.project.clone(),
            Some(request.task.clone()),
            kinds::TASK,
            &TaskEvent::Started {
                generation: "g1".into(),
            },
        )?;
        self.log.append(started).await?;
        self.spawned.lock().unwrap().push((host.clone(), request));
        Ok(())
    }

    async fn has(&self, _: &HostId, _: &ProjectId, task: &TaskId) -> Result<bool> {
        Ok(self
            .spawned
            .lock()
            .unwrap()
            .iter()
            .any(|(_, r)| &r.task == task))
    }
}

#[derive(Default)]
struct Guard(Mutex<Option<String>>);

#[async_trait]
impl MergeGuard for Guard {
    async fn main_health(&self, _: &ProjectId) -> Result<MainHealth> {
        Ok(MainHealth::Green)
    }
    async fn may_merge(&self, _: &Change, _: &Verdict) -> Result<Permit> {
        Ok(Permit::Allow)
    }
    async fn may_dispatch(&self, _: &ProjectId, _: &TaskId) -> Result<Permit> {
        Ok(match self.0.lock().unwrap().clone() {
            None => Permit::Allow,
            Some(reason) => Permit::Deny { reason },
        })
    }
}

#[derive(Default)]
struct Pool(Mutex<Vec<HostId>>);

#[async_trait]
impl IdlePool for Pool {
    async fn prune_idle(&self, host: &HostId) -> Result<String> {
        self.0.lock().unwrap().push(host.clone());
        Ok("pruned 2 idle worktrees".into())
    }
}

const RULES: &str = r#"{"classifier":{"provider":"system1"},
  "rules":[{"when":"anything","use":[{"harness":"claude","model":"claude-sonnet-5","pool":"team"},{"harness":"codex"}]}],
  "default":{"harness":"claude"}}"#;

fn host(id: &str, max: u32) -> Host {
    Host {
        id: id.into(),
        name: id.into(),
        runtime: RuntimeKind::Local,
        platform: Platform {
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        capacity: Capacity {
            cpus: 8,
            memory_bytes: 16 << 30,
            disk_bytes: 100 << 30,
            max_workers: max,
        },
        health: Health::Healthy,
        projects: Vec::new(),
        tasks: Vec::new(),
    }
}

fn quota(claude: f64, codex: f64) -> QuotaSnapshot {
    let p = |name: &str, sp: f64| {
        serde_json::json!({"provider": name, "quotaSemantics": {"status": "known", "effectiveAvailability": [
            {"scope": "all_models", "status": "known", "effectivePercentRemaining": 60,
             "runway": {"status": "ok"}, "selection": {"spendPriority": sp}}]}})
    };
    QuotaSnapshot::from_value(
        &serde_json::json!({"providers": [p("claude", claude), p("codex", codex)]}),
    )
    .unwrap()
}

fn answer(choice: &str, confidence: f64) -> Classification {
    Classification::Answered {
        answer: Answer {
            choice: choice.into(),
            confidence,
            probabilities: Default::default(),
            model: None,
            latency_ms: None,
            input_tokens: None,
            output_tokens: None,
        },
    }
}

fn request(title: &str) -> Request {
    Request {
        title: title.into(),
        brief: format!("Do {title}."),
        repo: PathBuf::from("/repo"),
        branch: format!("quark/{title}"),
        base: None,
        isolation: IsolationMode::Native,
        policy: Default::default(),
        env: BTreeMap::from([("A".to_string(), "1".to_string())]),
        profile: None,
    }
}

fn accounts() -> StaticAccounts {
    StaticAccounts(BTreeMap::from([(
        "claude".to_string(),
        vec![
            PoolAccount {
                id: "default-claude".into(),
                default: true,
                launchable: true,
                ready: true,
                ..PoolAccount::default()
            },
            PoolAccount {
                id: "acc_work".into(),
                pools: vec!["team".into()],
                launchable: true,
                ready: true,
                env: BTreeMap::from([("CLAUDE_CONFIG_DIR".to_string(), "/acc/work".to_string())]),
                ..PoolAccount::default()
            },
        ],
    )]))
}

struct Rig {
    log: MemoryEventLog,
    hosts: Arc<MemoryHosts>,
    fake: FakeHosts,
    guard: Arc<Guard>,
    pool: Arc<Pool>,
}

impl Rig {
    async fn new() -> Self {
        let log = MemoryEventLog::new();
        let hosts = Arc::new(MemoryHosts::default());
        hosts.register(host("mac", 1)).await.unwrap();
        Self {
            fake: FakeHosts {
                log: log.clone(),
                spawned: Arc::default(),
            },
            log,
            hosts,
            guard: Arc::default(),
            pool: Arc::default(),
        }
    }

    async fn dispatcher(
        &self,
        classification: Classification,
        unhealthy_after: Duration,
    ) -> Dispatcher {
        Dispatcher::open(
            Parts {
                log: Arc::new(self.log.clone()),
                host: "mac".into(),
                configs: Arc::new(FixedConfig(DispatchConfig::parse(RULES).unwrap())),
                classifier: Arc::new(Given(classification)),
                quota: Arc::new(FixedQuota(Some(quota(0.8, 0.3)))),
                families: ProviderFamilies::builtin(),
                accounts: Arc::new(accounts()),
                hosts: self.hosts.clone(),
                spawner: Arc::new(self.fake.clone()),
                guard: Some(self.guard.clone()),
                pool: Some(self.pool.clone()),
            },
            Settings {
                unhealthy_after,
                ..Settings::default()
            },
        )
        .await
        .unwrap()
    }

    fn kinds(&self) -> Vec<String> {
        self.log
            .events()
            .into_iter()
            .filter(|e| e.kind.prefix() == "dispatch")
            .map(|e| e.kind.0)
            .collect()
    }

    async fn sample(&self, host: &str, free_gib: u64) {
        let s = HostSample {
            host: host.into(),
            ts: OffsetDateTime::now_utc(),
            cpu: 0.1,
            memory_used_bytes: 0,
            memory_total_bytes: 16 << 30,
            memory_pressure: 0.1,
            disk_free_bytes: free_gib << 30,
            quark_disk: QuarkDisk::default(),
            usage: Vec::new(),
        };
        self.log
            .append(quark_hosts::recorder::sample_event(&s).unwrap())
            .await
            .unwrap();
    }

    async fn finish(&self, task: &str) {
        let e = NewEvent::typed(
            "mac".into(),
            "p".into(),
            Some(task.into()),
            kinds::TASK,
            &TaskEvent::Completed,
        )
        .unwrap();
        self.log.append(e).await.unwrap();
    }
}

#[tokio::test]
async fn resolves_chooses_an_account_and_places_by_capacity() {
    let rig = Rig::new().await;
    rig.sample("mac", 50).await;
    let d = rig
        .dispatcher(answer("rule_1", 0.9), Duration::from_secs(600))
        .await;

    let stage = d
        .submit("p".into(), "t1".into(), request("one"))
        .await
        .unwrap();
    let Stage::Chosen { choice } = stage else {
        panic!("{stage:?}");
    };
    assert_eq!(choice.profile.harness, "claude");
    assert_eq!(choice.account.as_deref(), Some("acc_work"));

    d.submit("p".into(), "t2".into(), request("two"))
        .await
        .unwrap();
    assert!(d
        .submit("p".into(), "t2".into(), request("two"))
        .await
        .is_err());

    let t = d.tick().await.unwrap();
    assert_eq!((t.placed, t.spawned, t.held), (1, 1, 1));
    {
        let spawned = rig.fake.spawned.lock().unwrap();
        let a = &spawned[0].1.assignment;
        assert_eq!(spawned[0].1.task.as_str(), "t1");
        assert_eq!(a.harness, "claude");
        assert_eq!(a.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(a.env["A"], "1");
        assert_eq!(a.env["CLAUDE_CONFIG_DIR"], "/acc/work");
    }
    // Still full: the hold is not recorded again.
    d.tick().await.unwrap();
    let held = rig.kinds().iter().filter(|k| *k == "dispatch.held").count();
    assert_eq!(held, 1);
    let pending = d.pending().await.unwrap();
    assert_eq!(
        pending[0].held.as_deref(),
        Some("host mac is full (1 of 1 workers)")
    );

    rig.finish("t1").await;
    let t = d.tick().await.unwrap();
    assert_eq!(t.spawned, 1);
    assert!(d.pending().await.unwrap().is_empty());
}

#[tokio::test]
async fn escalations_wait_for_the_coordinator() {
    let rig = Rig::new().await;
    let d = rig
        .dispatcher(answer("rule_1", 0.2), Duration::from_secs(600))
        .await;
    let stage = d
        .submit("p".into(), "t1".into(), request("one"))
        .await
        .unwrap();
    let Stage::Escalated { reason } = stage else {
        panic!("{stage:?}");
    };
    assert!(reason.contains("ambiguous"), "{reason}");
    assert_eq!(d.tick().await.unwrap().placed, 0);

    let mut codex = Profile::new("codex");
    codex.effort = Some("high".into());
    d.choose(&"p".into(), &"t1".into(), codex).await.unwrap();
    let t = d.tick().await.unwrap();
    assert_eq!(t.spawned, 1);
    // No sample yet: placed by worker count, with a note.
    let placed = rig
        .log
        .events()
        .into_iter()
        .find(|e| e.kind.as_str() == "dispatch.placed")
        .unwrap();
    let DispatchEvent::Placed { notes, .. } = placed.decode().unwrap() else {
        panic!();
    };
    assert!(notes[0].contains("no telemetry sample"));
}

#[tokio::test]
async fn red_main_holds_new_work() {
    let rig = Rig::new().await;
    let d = rig
        .dispatcher(answer("rule_1", 0.9), Duration::from_secs(600))
        .await;
    *rig.guard.0.lock().unwrap() = Some("main is red: ci failed at abc123".into());
    d.submit("p".into(), "t1".into(), request("one"))
        .await
        .unwrap();
    assert_eq!(d.tick().await.unwrap().held, 1);
    assert!(rig.fake.spawned.lock().unwrap().is_empty());
    *rig.guard.0.lock().unwrap() = None;
    assert_eq!(d.tick().await.unwrap().spawned, 1);
}

#[tokio::test]
async fn a_new_dispatcher_finishes_what_a_crash_left() {
    let rig = Rig::new().await;
    // A request recorded but never resolved.
    let e = NewEvent::typed(
        "mac".into(),
        "p".into(),
        Some("t1".into()),
        "dispatch.requested",
        &DispatchEvent::Requested {
            request: request("one"),
        },
    )
    .unwrap();
    rig.log.append(e).await.unwrap();
    let d = rig
        .dispatcher(answer("rule_1", 0.9), Duration::from_secs(600))
        .await;
    let t = d.tick().await.unwrap();
    assert_eq!((t.resolved, t.spawned), (1, 1));

    // A placement the host took but the dispatcher never confirmed.
    let mut lost = request("two");
    lost.profile = Some(Profile::new("codex"));
    rig.hosts.register(host("mac", 0)).await.unwrap();
    d.submit("p".into(), "t2".into(), lost).await.unwrap();
    let placed = NewEvent::typed(
        "mac".into(),
        "p".into(),
        Some("t2".into()),
        "dispatch.placed",
        &DispatchEvent::Placed {
            host: "mac".into(),
            notes: Vec::new(),
        },
    )
    .unwrap();
    rig.log.append(placed).await.unwrap();
    let again = rig
        .dispatcher(answer("rule_1", 0.9), Duration::from_secs(600))
        .await;
    let t = again.tick().await.unwrap();
    assert_eq!(t.spawned, 1);
    assert_eq!(rig.fake.spawned.lock().unwrap().len(), 2);
    assert!(again.pending().await.unwrap().is_empty());
}

#[tokio::test]
async fn prunes_low_disk_once_and_raises_unhealthy_hosts_once() {
    let rig = Rig::new().await;
    rig.hosts.register(host("ssh-box", 4)).await.unwrap();
    rig.sample("mac", 1).await;
    let d = rig.dispatcher(answer("rule_1", 0.9), Duration::ZERO).await;
    rig.hosts
        .set_health(
            &"ssh-box".into(),
            Health::Unreachable {
                reason: "no route".into(),
            },
        )
        .await
        .unwrap();
    d.submit("p".into(), "t1".into(), request("one"))
        .await
        .unwrap();

    let t = d.tick().await.unwrap();
    assert_eq!((t.pruned, t.alerts, t.held), (1, 1, 1));
    let t = d.tick().await.unwrap();
    assert_eq!((t.pruned, t.alerts), (0, 0));
    assert_eq!(rig.pool.0.lock().unwrap().len(), 1);

    rig.hosts
        .set_health(&"ssh-box".into(), Health::Healthy)
        .await
        .unwrap();
    rig.sample("mac", 50).await;
    let t = d.tick().await.unwrap();
    assert_eq!(t.spawned, 1);
    assert!(rig.kinds().contains(&"dispatch.host_recovered".to_string()));
}
