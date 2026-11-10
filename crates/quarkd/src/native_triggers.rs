//! Triggers, channels and the away policy (slice 7).
//!
//! With the native engine, quarkd runs the engine on a timer. It polls the
//! channel plug-ins in `<home>/channels` into the engine's inbox, evaluates
//! trigger rules, and routes each event by the away policy, acting through
//! [`DaemonEffects`]. The same engine serves the Automation API
//! (`api/automation.rs`). See `docs/engine/triggers.md`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quark_core::{CoreError, EventLog, NewEvent, ProjectId, TaskId};
use quark_systems::{EventType, Notice, NoticeItem, NoticeKind};
use quark_triggers::{Channel, CommandChannel, Effects, Engine, Item, Occasion, Processes};
use time::format_description::well_known::Rfc3339;
use tokio::sync::Mutex;

use crate::engine::{EngineAdapter, WorkspaceRef};
use crate::store::Store;

/// The slice 7 engine and its channel plug-ins.
pub struct Triggers {
    engine: Arc<Engine>,
    channels: Vec<CommandChannel>,
    next_poll: Mutex<HashMap<String, tokio::time::Instant>>,
}

impl Triggers {
    /// Open the engine, with the channel plug-ins in `channels_dir`.
    pub async fn open(
        store: Arc<Store>,
        log: Arc<dyn EventLog>,
        adapter: Arc<dyn EngineAdapter>,
        channels_dir: &Path,
    ) -> anyhow::Result<Self> {
        let effects: Arc<dyn Effects> = Arc::new(DaemonEffects {
            store,
            log: log.clone(),
            adapter,
        });
        let engine = Engine::open(log, crate::host(), effects, Arc::new(Processes)).await?;
        let (channels, errors) = CommandChannel::load_dir(channels_dir, Arc::new(Processes));
        for e in errors {
            tracing::warn!(error = %e, "channel plug-in skipped");
        }
        if !channels.is_empty() {
            let names: Vec<&str> = channels.iter().map(|c| c.name.as_str()).collect();
            tracing::info!(channels = ?names, "channel plug-ins loaded");
        }
        Ok(Self {
            engine: Arc::new(engine),
            channels,
            next_poll: Mutex::new(HashMap::new()),
        })
    }

    /// The engine, for the API.
    pub fn engine(&self) -> Arc<Engine> {
        self.engine.clone()
    }

    /// Poll the channels that are due, then evaluate.
    pub async fn pass(&self) -> anyhow::Result<()> {
        self.poll_channels().await;
        self.engine.tick(time::OffsetDateTime::now_utc()).await?;
        Ok(())
    }

    async fn poll_channels(&self) {
        let now = tokio::time::Instant::now();
        for channel in &self.channels {
            {
                let mut next = self.next_poll.lock().await;
                if next.get(&channel.name).is_some_and(|n| now < *n) {
                    continue;
                }
                next.insert(channel.name.clone(), now + channel.interval());
            }
            if let Err(e) = self.poll_channel(channel).await {
                tracing::warn!(channel = %channel.name, error = %e, "channel poll failed");
            }
        }
    }

    /// Receive what `channel` has that the engine has not seen.
    async fn poll_channel(&self, channel: &CommandChannel) -> anyhow::Result<()> {
        let project = channel.project().clone();
        for m in channel.poll().await? {
            if self.engine.knows(&project, &m.channel, &m.id).await {
                continue;
            }
            self.engine.receive(&project, m).await?;
        }
        Ok(())
    }

    pub async fn run(self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(e) = self.pass().await {
                tracing::error!(error = %format!("{e:#}"), "slice 7 pass failed");
            }
        }
    }
}

async fn workspace(store: &Arc<Store>, project: &ProjectId) -> quark_core::Result<WorkspaceRef> {
    let store = store.clone();
    let id = project.to_string();
    let p = tokio::task::spawn_blocking(move || store.get_project(&id))
        .await
        .map_err(|e| CoreError::Backend(e.to_string()))?
        .map_err(|e| CoreError::NotFound(e.to_string()))?;
    let root = p
        .workspace_path
        .ok_or_else(|| CoreError::Invalid(format!("Project {project} has no workspace yet")))?;
    Ok(WorkspaceRef {
        project_id: p.id,
        root: root.into(),
    })
}

/// What native mode does in the world:
///
/// - a rule's `wake` asks the coordinator for a turn (`coordinator.requested`
///   in the log, which slice 6 wakes on);
/// - telling the user, now or as a digest, is a `notice.posted` event on
///   the daemon's stream, which the app shows;
/// - steering goes to the task's worker through the engine adapter.
pub struct DaemonEffects {
    store: Arc<Store>,
    log: Arc<dyn EventLog>,
    adapter: Arc<dyn EngineAdapter>,
}

impl DaemonEffects {
    async fn post(&self, notice: Notice) -> quark_core::Result<()> {
        let store = self.store.clone();
        let project = notice.project_id.clone();
        let payload =
            serde_json::to_value(&notice).map_err(|e| CoreError::Invalid(e.to_string()))?;
        tokio::task::spawn_blocking(move || {
            store.emit(Some(&project), EventType::NoticePosted, payload)
        })
        .await
        .map_err(|e| CoreError::Backend(e.to_string()))?
        .map_err(|e| CoreError::Backend(e.to_string()))
    }

    /// The API ids of the tasks the engine knows by these names, so a notice
    /// links to `GET /v1/tasks/{id}` and the app's `#/t/<id>`. A name the
    /// store does not know is kept as it is.
    async fn api_ids(&self, project: &ProjectId) -> HashMap<String, String> {
        let store = self.store.clone();
        let pid = project.to_string();
        match tokio::task::spawn_blocking(move || store.task_ids_by_engine(&pid)).await {
            Ok(Ok(ids)) => ids,
            Ok(Err(e)) => {
                tracing::warn!(project = %project, error = %e, "could not read task ids for a notice");
                HashMap::new()
            }
            Err(e) => {
                tracing::warn!(project = %project, error = %e, "could not read task ids for a notice");
                HashMap::new()
            }
        }
    }
}

/// A notice's `task_id` (the API id when known) and `worker` (the engine's name).
fn task_ref(
    ids: &HashMap<String, String>,
    task: Option<&TaskId>,
) -> (Option<String>, Option<String>) {
    match task {
        None => (None, None),
        Some(t) => {
            let name = t.to_string();
            (
                Some(ids.get(&name).cloned().unwrap_or_else(|| name.clone())),
                Some(name),
            )
        }
    }
}

#[async_trait]
impl Effects for DaemonEffects {
    async fn wake(
        &self,
        project: &ProjectId,
        task: Option<&TaskId>,
        note: &str,
    ) -> quark_core::Result<()> {
        let ev = quark_coordinator::CoordinatorEvent::Requested {
            note: note.to_string(),
            by: "trigger".into(),
        };
        let event = NewEvent::typed(
            crate::host(),
            project.clone(),
            task.cloned(),
            ev.kind(),
            &ev,
        )?;
        self.log.append(event).await.map(drop)
    }

    async fn notify(
        &self,
        project: &ProjectId,
        task: Option<&TaskId>,
        occasion: Occasion,
        note: &str,
    ) -> quark_core::Result<()> {
        let ids = match task {
            Some(_) => self.api_ids(project).await,
            None => HashMap::new(),
        };
        let (task_id, worker) = task_ref(&ids, task);
        self.post(Notice {
            project_id: project.to_string(),
            kind: NoticeKind::Now,
            text: note.to_string(),
            task_id,
            worker,
            occasion: Some(crate::api::automation::occasion_out(occasion)),
            items: Vec::new(),
        })
        .await
    }

    async fn digest(
        &self,
        project: &ProjectId,
        items: &[Item],
        returning: bool,
    ) -> quark_core::Result<()> {
        let n = items.len();
        let s = if n == 1 { "" } else { "s" };
        let ids = if items.iter().any(|i| i.task.is_some()) {
            self.api_ids(project).await
        } else {
            HashMap::new()
        };
        self.post(Notice {
            project_id: project.to_string(),
            kind: if returning {
                NoticeKind::Return
            } else {
                NoticeKind::Digest
            },
            text: if returning {
                format!("While you were away: {n} update{s}")
            } else {
                format!("{n} update{s} since the last digest")
            },
            task_id: None,
            worker: None,
            occasion: None,
            items: items
                .iter()
                .map(|i| {
                    let (task_id, worker) = task_ref(&ids, i.task.as_ref());
                    NoticeItem {
                        occasion: crate::api::automation::occasion_out(i.occasion),
                        summary: i.summary.clone(),
                        task_id,
                        worker,
                        at: i.at.format(&Rfc3339).unwrap_or_default(),
                    }
                })
                .collect(),
        })
        .await
    }

    async fn steer(
        &self,
        project: &ProjectId,
        task: &TaskId,
        text: &str,
    ) -> quark_core::Result<()> {
        let ws = workspace(&self.store, project).await?;
        self.adapter
            .send_message(&ws, task.as_str(), text)
            .await
            .map_err(|e| CoreError::Backend(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use quark_core::Seq;
    use quark_systems::{CreateProject, TaskState};

    use super::*;
    use crate::engine::{EngineTask, FleetSnapshot, StubEngine, StubWrite};

    fn messages(stub: &StubEngine) -> Vec<(String, String)> {
        stub.writes()
            .into_iter()
            .filter_map(|w| match w {
                StubWrite::Message { task_id, text } => Some((task_id, text)),
                _ => None,
            })
            .collect()
    }

    fn project(store: &Store, home: &Path) -> String {
        store
            .create_project(CreateProject {
                name: "Quark".into(),
                goal: None,
                workspace_path: Some(home.display().to_string()),
                repos: vec![],
                agent_config: None,
                dispatch_preset: None,
                delivery: None,
            })
            .unwrap()
            .id
    }

    fn channel(dir: &Path, project: &str) -> PathBuf {
        let chans = dir.join("channels");
        std::fs::create_dir_all(&chans).unwrap();
        let script = dir.join("mail.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho '{\"id\":\"m1\",\"body\":\"disk alert\",\"from\":\"ops\"}'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            chans.join("mail.json"),
            format!(
                r#"{{"project":"{project}","argv":["{}"],"interval_secs":1}}"#,
                script.display()
            ),
        )
        .unwrap();
        chans
    }

    #[tokio::test]
    async fn native_effects_reach_the_coordinator_the_user_and_the_worker() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let pid = project(&store, &home);
        let stub = Arc::new(StubEngine::default());
        let log = quark_eventlog::SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let chans = channel(dir.path(), &pid);
        let triggers = Triggers::open(store.clone(), Arc::new(log.clone()), stub.clone(), &chans)
            .await
            .unwrap();
        let effects = DaemonEffects {
            store: store.clone(),
            log: Arc::new(log.clone()),
            adapter: stub.clone(),
        };
        // The engine knows the task as t1; the store and the app by its API id.
        store
            .apply_snapshot(
                &pid,
                &FleetSnapshot {
                    tasks: vec![EngineTask {
                        id: "t1".into(),
                        title: "t1 title".into(),
                        kind: None,
                        state: TaskState::Done,
                        state_note: None,
                        state_source: None,
                        harness: None,
                        pull_request_url: None,
                        terminal: None,
                        worktree: None,
                        worktree_at: None,
                        unhealthy: None,
                        unanswered_since: None,
                    }],
                },
            )
            .unwrap();
        let api_id = store.task_id_for_engine(&pid, "t1").unwrap().unwrap();
        assert_ne!(api_id, "t1");
        let p = ProjectId::new(pid.clone());
        effects.wake(&p, None, "check the nightly").await.unwrap();
        effects
            .notify(&p, Some(&TaskId::from("t1")), Occasion::Done, "t1: done")
            .await
            .unwrap();
        effects
            .notify(
                &p,
                Some(&TaskId::from("gone")),
                Occasion::Done,
                "gone: done",
            )
            .await
            .unwrap();
        effects
            .steer(&p, &TaskId::from("t1"), "rebase please")
            .await
            .unwrap();
        let requested: Vec<_> = log
            .read(Seq::ZERO, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind.as_str() == "coordinator.requested")
            .collect();
        assert_eq!(requested.len(), 1);
        assert_eq!(requested[0].payload["note"], "check the nightly");
        let notices: Vec<_> = store
            .events_after(0, 1000)
            .unwrap()
            .into_iter()
            .filter(|e| e.event_type == EventType::NoticePosted)
            .collect();
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].payload["text"], "t1: done");
        assert_eq!(notices[0].payload["kind"], "now");
        assert_eq!(notices[0].payload["occasion"], "done");
        // The link goes to the task's API id, not the engine's name for it.
        assert_eq!(notices[0].payload["task_id"], api_id.as_str());
        assert_eq!(notices[0].payload["worker"], "t1");
        // A name the store does not know is passed on as it is.
        assert_eq!(notices[1].payload["task_id"], "gone");
        assert_eq!(notices[1].payload["worker"], "gone");
        assert_eq!(
            messages(&stub),
            [("t1".to_string(), "rebase please".to_string())]
        );

        // Channel messages land in the engine's own inbox.
        triggers.pass().await.unwrap();
        assert_eq!(triggers.engine().inbox(&p).await.len(), 1);
    }
}
