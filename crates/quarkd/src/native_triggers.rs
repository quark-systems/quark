//! Triggers, channels and the away policy (slice 7).
//!
//! Slice 7 switches on after slices 1 to 6, so until then firstmate owns
//! every Project's inbox, away posture and watches. Unless [`ENV`] is `0`,
//! quarkd runs the engine on a timer. In shadow mode (the default) it:
//!
//! - mirrors each Project's firstmate inbox notes and away posture into
//!   the event log;
//! - mirrors each firstmate when-then watch as a rule, runs its condition
//!   too, and records a `shadow.divergence` when the two end differently;
//! - compares its route for each firstmate status line with firstmate's
//!   away classifier, recording disagreements as `shadow.divergence`;
//! - evaluates trigger rules and records what would have fired;
//! - polls the channel plug-ins in `<home>/channels` and hands what
//!   arrives to firstmate's inbox, so today's coordinator reads it.
//!
//! It never wakes, notifies or steers anyone. Once slice 7 is native
//! (`QUARK_ENGINE_SLICES`), it acts through [`DaemonEffects`] and keeps
//! channel messages in its own inbox instead. The same engine serves the
//! Automation API (`api/automation.rs`). See `docs/engine/triggers.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quark_core::{CoreError, EventLog, NewEvent, ProjectId, TaskId};
use quark_systems::{EventType, Notice, NoticeItem, NoticeKind};
use quark_triggers::{Channel, CommandChannel, Effects, Engine, Item, Mode, NoEffects, Processes};
use time::format_description::well_known::Rfc3339;
use tokio::sync::Mutex;

use crate::engine::{EngineAdapter, WorkspaceRef};
use crate::store::Store;

/// Set to `0` to turn the slice 7 engine off.
pub const ENV: &str = "QUARK_NATIVE_TRIGGERS";
/// Who the engine records as handling a channel message it handed to
/// firstmate's inbox.
const FORWARDED: &str = "firstmate";

pub fn enabled() -> bool {
    !std::env::var(ENV).is_ok_and(|v| v == "0")
}

/// The slice 7 engine, its channel plug-ins and the Projects it mirrors.
pub struct Triggers {
    engine: Arc<Engine>,
    store: Arc<Store>,
    adapter: Arc<dyn EngineAdapter>,
    channels: Vec<CommandChannel>,
    next_poll: Mutex<HashMap<String, tokio::time::Instant>>,
}

impl Triggers {
    /// Open the engine in `mode`, with the channel plug-ins in
    /// `channels_dir`.
    pub async fn open(
        store: Arc<Store>,
        log: Arc<dyn EventLog>,
        adapter: Arc<dyn EngineAdapter>,
        mode: Mode,
        channels_dir: &Path,
    ) -> anyhow::Result<Self> {
        let effects: Arc<dyn Effects> = match mode {
            Mode::Shadow => Arc::new(NoEffects),
            Mode::Native => Arc::new(DaemonEffects {
                store: store.clone(),
                log: log.clone(),
                adapter: adapter.clone(),
            }),
        };
        let engine = Engine::open(
            log,
            crate::event_ingest::host(),
            mode,
            effects,
            Arc::new(Processes),
        )
        .await?;
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
            store,
            adapter,
            channels,
            next_poll: Mutex::new(HashMap::new()),
        })
    }

    /// The engine, for the API.
    pub fn engine(&self) -> Arc<Engine> {
        self.engine.clone()
    }

    /// Mirror every Project's firstmate home (shadow), poll the channels
    /// that are due, then evaluate.
    pub async fn pass(&self) -> anyhow::Result<()> {
        if self.engine.mode() == Mode::Shadow {
            self.mirror().await?;
        }
        self.poll_channels().await;
        let r = self.engine.tick(time::OffsetDateTime::now_utc()).await?;
        if r.diverged > 0 {
            tracing::info!(
                compared = r.compared,
                diverged = r.diverged,
                "slice 7 shadow disagreed with firstmate"
            );
        }
        Ok(())
    }

    async fn mirror(&self) -> anyhow::Result<()> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects()).await??;
        for project in projects {
            let Some(path) = project.workspace_path else {
                continue;
            };
            let id = ProjectId::new(project.id);
            match self
                .engine
                .mirror_firstmate(&id, &PathBuf::from(path))
                .await
            {
                Ok(r) if r.diverged > 0 => tracing::info!(
                    project = %id,
                    agreed = r.agreed,
                    diverged = r.diverged,
                    "slice 7 when-watch shadow disagreed with firstmate"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(project = %id, error = %e, "slice 7 mirror failed"),
            }
        }
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

    /// Receive what `channel` has that the engine has not seen. In shadow
    /// mode each new message first goes to firstmate's inbox, then is
    /// recorded as handled there, so it reaches today's coordinator once.
    /// A crash between the two hands it to firstmate again on the next
    /// poll.
    async fn poll_channel(&self, channel: &CommandChannel) -> anyhow::Result<()> {
        let project = channel.project().clone();
        for m in channel.poll().await? {
            if self.engine.knows(&project, &m.channel, &m.id).await {
                continue;
            }
            if self.engine.mode() == Mode::Shadow {
                let ws = workspace(&self.store, &project).await?;
                let text = format!("{} from {}: {}", m.channel, m.from, m.body);
                self.adapter.inbox_note(&ws, &text).await?;
                let (ch, id) = (m.channel.clone(), m.id.clone());
                self.engine.receive(&project, m).await?;
                self.engine.ack(&project, &ch, &id, FORWARDED).await?;
            } else {
                self.engine.receive(&project, m).await?;
            }
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
            crate::event_ingest::host(),
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
        note: &str,
    ) -> quark_core::Result<()> {
        self.post(Notice {
            project_id: project.to_string(),
            kind: NoticeKind::Now,
            text: note.to_string(),
            task_id: task.map(|t| t.to_string()),
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
            items: items
                .iter()
                .map(|i| NoticeItem {
                    occasion: crate::api::automation::occasion_out(i.occasion),
                    summary: i.summary.clone(),
                    task_id: i.task.as_ref().map(|t| t.to_string()),
                    at: i.at.format(&Rfc3339).unwrap_or_default(),
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
    use quark_core::Seq;
    use quark_systems::CreateProject;

    use super::*;
    use crate::engine::{StubEngine, StubWrite};

    fn notes(stub: &StubEngine) -> Vec<String> {
        stub.writes()
            .into_iter()
            .filter_map(|w| match w {
                StubWrite::InboxNote { text } => Some(text),
                _ => None,
            })
            .collect()
    }

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
    async fn a_shadow_pass_mirrors_each_project_and_acts_on_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join("state/inbox")).unwrap();
        std::fs::write(
            home.join("state/inbox/1-a.note"),
            "id=1-a\nat=2026-10-07T01:00:00Z\nsource=cli\n--\ncheck the nightly\n",
        )
        .unwrap();
        std::fs::write(home.join("state/.afk"), "quiet\n").unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let pid = project(&store, &home);
        let log = quark_eventlog::SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let triggers = Triggers::open(
            store,
            Arc::new(log.clone()),
            Arc::new(StubEngine::default()),
            Mode::Shadow,
            &dir.path().join("no-channels"),
        )
        .await
        .unwrap();
        triggers.pass().await.unwrap();
        triggers.pass().await.unwrap();
        let kinds: Vec<String> = log
            .read(Seq::ZERO, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.project.as_str() == pid)
            .map(|e| e.kind.0)
            .collect();
        assert_eq!(kinds, ["channel.received", "away.posture"]);
    }

    #[tokio::test]
    async fn shadow_hands_channel_messages_to_firstmate_once() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let pid = project(&store, &home);
        let chans = channel(dir.path(), &pid);
        let stub = Arc::new(StubEngine::default());
        let log = quark_eventlog::SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        let triggers = Triggers::open(
            store,
            Arc::new(log.clone()),
            stub.clone(),
            Mode::Shadow,
            &chans,
        )
        .await
        .unwrap();
        triggers.poll_channel(&triggers.channels[0]).await.unwrap();
        triggers.poll_channel(&triggers.channels[0]).await.unwrap();
        assert_eq!(notes(&stub), ["mail from ops: disk alert"]);
        let engine = triggers.engine();
        assert!(
            engine
                .knows(&ProjectId::new(pid.clone()), "mail", "m1")
                .await
        );
        assert!(engine.inbox(&ProjectId::new(pid)).await.is_empty());
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
        let triggers = Triggers::open(
            store.clone(),
            Arc::new(log.clone()),
            stub.clone(),
            Mode::Native,
            &chans,
        )
        .await
        .unwrap();
        let effects = DaemonEffects {
            store: store.clone(),
            log: Arc::new(log.clone()),
            adapter: stub.clone(),
        };
        let p = ProjectId::new(pid.clone());
        effects.wake(&p, None, "check the nightly").await.unwrap();
        effects
            .notify(&p, Some(&TaskId::from("t1")), "t1: done")
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
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].payload["text"], "t1: done");
        assert_eq!(notices[0].payload["kind"], "now");
        assert_eq!(
            messages(&stub),
            [("t1".to_string(), "rebase please".to_string())]
        );

        // Native channels land in the engine's own inbox, not firstmate's.
        triggers.pass().await.unwrap();
        assert!(notes(&stub).is_empty());
        assert_eq!(triggers.engine().inbox(&p).await.len(), 1);
    }
}
