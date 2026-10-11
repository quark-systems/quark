//! Channels: one inbound API for everything that reaches a Project from
//! outside a task.
//!
//! The user's inbox is the first channel: a note dropped in while the
//! coordinator is busy, kept until it is handled. Email, voice and public
//! mentions come later as plug-in [`Channel`]s that the engine polls; they
//! all land the same way, as a `channel.received` event, and leave the
//! pending set with `channel.acked`.

use std::collections::BTreeMap;

use async_trait::async_trait;
use quark_core::{CoreError, Event, EventKind, ProjectId, Result, TaskId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The prefix every channel event kind starts with.
pub const PREFIX: &str = "channel";

/// The built-in channel for the user's own notes.
pub const INBOX: &str = "inbox";

/// One message from a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inbound {
    /// Stable within its channel: receiving the same id twice is a no-op.
    pub id: String,
    /// `inbox`, or a plug-in channel's name.
    pub channel: String,
    /// Who or what sent it, such as `user`, `voice` or an address.
    pub from: String,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The task it is about, when the sender said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
}

impl Inbound {
    /// A note for the inbox, stamped now with a fresh id.
    pub fn note(from: impl Into<String>, body: impl Into<String>) -> Self {
        let at = OffsetDateTime::now_utc();
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            channel: INBOX.to_string(),
            from: from.into(),
            body: body.into(),
            at,
            task: None,
        }
    }

    /// The first line, cut to `max` characters, for digests and wakes.
    pub fn summary(&self, max: usize) -> String {
        let line = self
            .body
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        let line = line.trim();
        if line.chars().count() <= max {
            line.to_string()
        } else {
            let cut: String = line.chars().take(max.saturating_sub(1)).collect();
            format!("{cut}…")
        }
    }
}

/// Payload of every `channel.*` event; the kind is `channel.<type>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelEvent {
    /// A message arrived and is pending until acknowledged.
    Received { message: Inbound },
    /// A pending message was handled.
    Acked {
        channel: String,
        id: String,
        /// Who handled it: `user`, `coordinator`, or `firstmate` when
        /// mirrored from a bash home.
        by: String,
    },
}

impl ChannelEvent {
    pub fn kind(&self) -> EventKind {
        let name = match self {
            ChannelEvent::Received { .. } => "received",
            ChannelEvent::Acked { .. } => "acked",
        };
        EventKind::new(format!("{PREFIX}.{name}"))
    }

    /// The deterministic event id, so the same message or acknowledgement
    /// appended twice (a retry, a re-read bash file) lands once.
    pub fn event_id(&self, project: &ProjectId) -> quark_core::EventId {
        match self {
            ChannelEvent::Received { message } => crate::stable_id(&[
                "channel.received",
                project.as_str(),
                &message.channel,
                &message.id,
            ]),
            ChannelEvent::Acked { channel, id, .. } => {
                crate::stable_id(&["channel.acked", project.as_str(), channel, id])
            }
        }
    }
}

/// A plug-in channel the engine polls, such as email. Each call returns
/// what arrived since the last; ids must be stable so a message returned
/// twice is received once.
#[async_trait]
pub trait Channel: Send + Sync {
    /// The channel name messages carry.
    fn name(&self) -> &str;

    /// The Project messages are delivered to.
    fn project(&self) -> &ProjectId;

    async fn poll(&self) -> Result<Vec<Inbound>>;
}

/// A channel plug-in as a program: each poll runs `argv` (no shell) and
/// reads one JSON message per line of its output,
/// `{"id", "body", "from"?, "at"?, "task"?}`. Exit 0 with no output means
/// nothing new; any other exit is an error. Ids must be stable, so the
/// program need not remember what it already returned, but it may.
///
/// quarkd loads one per `<home>/channels/<name>.json` manifest
/// ([`CommandChannel::load_dir`]), so email, voice or mentions are a
/// script away.
#[derive(Clone)]
pub struct CommandChannel {
    pub name: String,
    pub project: ProjectId,
    pub manifest: ChannelManifest,
    commands: std::sync::Arc<dyn crate::Commands>,
}

/// `<home>/channels/<name>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelManifest {
    /// The Project messages are delivered to.
    pub project: String,
    pub argv: Vec<String>,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_interval() -> u64 {
    60
}
fn default_timeout() -> u64 {
    30
}

/// One line of a plug-in's output.
#[derive(Debug, Deserialize)]
struct Line {
    id: String,
    body: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    at: Option<OffsetDateTime>,
    #[serde(default)]
    task: Option<TaskId>,
}

impl CommandChannel {
    pub fn new(
        name: impl Into<String>,
        manifest: ChannelManifest,
        commands: std::sync::Arc<dyn crate::Commands>,
    ) -> Result<Self> {
        let name = name.into();
        let bad = |m: &str| Err(CoreError::Invalid(format!("channel {name}: {m}")));
        if name.is_empty()
            || name.len() > 32
            || !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return bad("the name must be 1-32 lowercase letters, digits, '-' or '_'");
        }
        if name == INBOX {
            return bad("`inbox` is the built-in channel");
        }
        if manifest.project.is_empty() {
            return bad("no project");
        }
        if manifest.argv.first().is_none_or(|a| a.is_empty()) {
            return bad("the command is empty");
        }
        if manifest.interval_secs == 0 || manifest.timeout_secs == 0 {
            return bad("interval and timeout must be at least 1");
        }
        Ok(Self {
            project: ProjectId::new(manifest.project.clone()),
            name,
            manifest,
            commands,
        })
    }

    /// Every `*.json` manifest in `dir`, and why each one that could not
    /// be loaded was skipped. A missing directory has none.
    pub fn load_dir(
        dir: &std::path::Path,
        commands: std::sync::Arc<dyn crate::Commands>,
    ) -> (Vec<CommandChannel>, Vec<String>) {
        let (mut ok, mut errors) = (Vec::new(), Vec::new());
        let Ok(entries) = std::fs::read_dir(dir) else {
            return (ok, errors);
        };
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for path in paths {
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let loaded = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| {
                    serde_json::from_str::<ChannelManifest>(&t).map_err(|e| e.to_string())
                })
                .and_then(|m| Self::new(&name, m, commands.clone()).map_err(|e| e.to_string()));
            match loaded {
                Ok(c) => ok.push(c),
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
        (ok, errors)
    }

    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.manifest.interval_secs)
    }
}

#[async_trait]
impl Channel for CommandChannel {
    fn name(&self) -> &str {
        &self.name
    }

    fn project(&self) -> &ProjectId {
        &self.project
    }

    async fn poll(&self) -> Result<Vec<Inbound>> {
        let ran = self
            .commands
            .run(
                &self.manifest.argv,
                std::time::Duration::from_secs(self.manifest.timeout_secs),
            )
            .await;
        if ran.code != Some(0) || ran.timed_out {
            let why = if ran.stderr.trim().is_empty() {
                format!("exit {:?}", ran.code)
            } else {
                ran.stderr.trim().to_string()
            };
            return Err(CoreError::Backend(format!("channel {}: {why}", self.name)));
        }
        let now = OffsetDateTime::now_utc();
        let mut out = Vec::new();
        for (n, line) in ran.stdout.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let l: Line = serde_json::from_str(line).map_err(|e| {
                CoreError::Invalid(format!("channel {} line {}: {e}", self.name, n + 1))
            })?;
            if l.id.is_empty() || l.body.trim().is_empty() {
                continue;
            }
            out.push(Inbound {
                id: l.id,
                channel: self.name.clone(),
                from: l.from.unwrap_or_else(|| self.name.clone()),
                body: l.body,
                at: l.at.unwrap_or(now),
                task: l.task,
            });
        }
        Ok(out)
    }
}

/// A message's key within a Project: (channel, id).
type Key = (String, String);

/// Pending messages per Project, folded from `channel.*` events.
#[derive(Debug, Clone, Default)]
pub struct Inboxes {
    /// Project -> (channel, id) -> message, oldest first by arrival.
    pending: BTreeMap<ProjectId, BTreeMap<Key, (u64, Inbound)>>,
    acked: BTreeMap<ProjectId, std::collections::BTreeSet<Key>>,
}

impl Inboxes {
    /// Apply one event; anything that is not a channel event is ignored.
    /// Safe to apply the same event twice.
    pub fn apply(&mut self, e: &Event) {
        if e.kind.prefix() != PREFIX {
            return;
        }
        let Ok(ev) = e.decode::<ChannelEvent>() else {
            tracing::warn!(
                seq = e.seq.0,
                kind = e.kind.as_str(),
                "unreadable channel event"
            );
            return;
        };
        match ev {
            ChannelEvent::Received { message } => {
                let key = (message.channel.clone(), message.id.clone());
                if self.acked.get(&e.project).is_some_and(|a| a.contains(&key)) {
                    return;
                }
                self.pending
                    .entry(e.project.clone())
                    .or_default()
                    .entry(key)
                    .or_insert((e.seq.0, message));
            }
            ChannelEvent::Acked { channel, id, .. } => {
                let key = (channel, id);
                if let Some(p) = self.pending.get_mut(&e.project) {
                    p.remove(&key);
                }
                self.acked.entry(e.project.clone()).or_default().insert(key);
            }
        }
    }

    /// Pending messages of `project`, in arrival order.
    pub fn pending(&self, project: &ProjectId) -> Vec<Inbound> {
        let mut v: Vec<_> = self
            .pending
            .get(project)
            .map(|p| p.values().cloned().collect())
            .unwrap_or_default();
        v.sort_by_key(|(seq, _)| *seq);
        v.into_iter().map(|(_, m)| m).collect()
    }

    pub fn is_pending(&self, project: &ProjectId, channel: &str, id: &str) -> bool {
        self.pending
            .get(project)
            .is_some_and(|p| p.contains_key(&(channel.to_string(), id.to_string())))
    }

    pub fn was_acked(&self, project: &ProjectId, channel: &str, id: &str) -> bool {
        self.acked
            .get(project)
            .is_some_and(|a| a.contains(&(channel.to_string(), id.to_string())))
    }
}

#[cfg(test)]
mod tests {
    use quark_core::{HostId, NewEvent, Seq};

    use super::*;

    fn ev(project: &str, seq: u64, c: &ChannelEvent) -> Event {
        let p = ProjectId::from(project);
        NewEvent::typed(HostId::from("h"), p, None, c.kind(), c)
            .unwrap()
            .with_seq(Seq(seq))
    }

    #[test]
    fn received_then_acked() {
        let m = Inbound::note("user", "look at the flaky test\nmore");
        let mut ib = Inboxes::default();
        let r = ChannelEvent::Received { message: m.clone() };
        ib.apply(&ev("p", 1, &r));
        ib.apply(&ev("p", 1, &r));
        assert_eq!(ib.pending(&"p".into()), vec![m.clone()]);
        assert!(ib.pending(&"q".into()).is_empty());
        ib.apply(&ev(
            "p",
            2,
            &ChannelEvent::Acked {
                channel: INBOX.into(),
                id: m.id.clone(),
                by: "user".into(),
            },
        ));
        assert!(ib.pending(&"p".into()).is_empty());
        // A late re-delivery of an acked message stays handled.
        ib.apply(&ev("p", 3, &r));
        assert!(ib.pending(&"p".into()).is_empty());
        assert_eq!(m.summary(9), "look at …");
    }

    #[tokio::test]
    async fn a_program_is_a_channel() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("mail.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' '{\"id\":\"m1\",\"body\":\"disk alert\",\"from\":\"ops@example\"}' '' '{\"id\":\"m2\",\"body\":\"  \"}'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let chans = dir.path().join("channels");
        std::fs::create_dir_all(&chans).unwrap();
        std::fs::write(
            chans.join("mail.json"),
            format!(r#"{{"project":"p","argv":["{}"]}}"#, script.display()),
        )
        .unwrap();
        std::fs::write(chans.join("inbox.json"), r#"{"project":"p","argv":["x"]}"#).unwrap();
        std::fs::write(chans.join("bad.json"), "{").unwrap();
        let (ok, errors) = CommandChannel::load_dir(&chans, std::sync::Arc::new(crate::Processes));
        assert_eq!(ok.len(), 1);
        assert_eq!(errors.len(), 2, "{errors:?}");
        let got = ok[0].poll().await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].channel, "mail");
        assert_eq!(got[0].from, "ops@example");
        assert_eq!(got[0].body, "disk alert");

        let failing = CommandChannel::new(
            "voice",
            ChannelManifest {
                project: "p".into(),
                argv: vec!["false".into()],
                interval_secs: 1,
                timeout_secs: 1,
            },
            std::sync::Arc::new(crate::Processes),
        )
        .unwrap();
        assert!(failing.poll().await.is_err());
    }

    #[test]
    fn ids_are_stable() {
        let m = Inbound::note("user", "x");
        let r = ChannelEvent::Received { message: m };
        assert_eq!(r.event_id(&"p".into()), r.event_id(&"p".into()));
        assert_ne!(r.event_id(&"p".into()), r.event_id(&"q".into()));
        assert_eq!(r.kind().as_str(), "channel.received");
    }
}
