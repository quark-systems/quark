//! Compacting a Project coordinator's conversation, and how full its
//! context is.
//!
//! A harness compacts on its own only when the context window is nearly
//! full: close to a million tokens on a 1M model, every turn of which reads
//! the whole context again. So Quark compacts earlier. When a coordinator
//! turn ends with its context at the Project's threshold or more
//! ([`CompactionSettings`], default [`COMPACT_AT_TOKENS`]), the engine asks
//! it to wrap up (save what only its conversation holds); when that turn
//! ends, it types the harness's `/compact` with what the summary must keep.
//! Every coordinator harness (Claude Code, Codex, Pi) has `/compact`. The
//! compaction then shows in the conversation as the entry the session log
//! records for it, whoever started it.
//!
//! [`Compactor`] keeps where each coordinator's compaction is, in memory: a
//! restart forgets a compaction under way, and the next full turn starts
//! one again. It also reads each coordinator's newest session log for the
//! context its last turn ended on, for the coordinator's header
//! ([`CoordinatorContext`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use quark_systems::{
    AgentConfig, CompactionPhase, CompactionSettings, CoordinatorContext, Project,
};
use quark_transcript::{locate, read_turns, SessionFormat, SessionRoots, UsageCursor};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::chat::{ChatError, CoordinatorInput, Delivery};
use crate::coordinator_commands::{self, Asked, CommandError};
use crate::engine::WorkspaceRef;
use crate::store::Store;

/// The context size, in tokens, at which Quark compacts a coordinator that
/// has no setting of its own: 70% of a 200k window, leaving room for the
/// wrap up before the harness would compact on its own.
pub const COMPACT_AT_TOKENS: u64 = 140_000;

/// What the summary must keep, passed to `/compact`.
pub const KEEP: &str = "Keep the user's standing rules and preferences, the open decisions, \
    every task under way with its id, branch and pull request, what you promised and have not \
    done yet, and what you were about to do next.";

/// How long a `/compact` may go unseen before Quark stops waiting for it.
const COMPACT_TIMEOUT: time::Duration = time::Duration::minutes(15);

/// What follows a coordinator turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Nothing,
    /// Ask for a wrap up: the context holds `tokens`.
    WrapUp {
        tokens: u64,
    },
    /// Type `/compact`.
    Compact,
}

/// Where each coordinator's compaction is, and what its log last said.
#[derive(Debug)]
pub struct Compactor {
    default: CompactionSettings,
    roots: SessionRoots,
    live: Mutex<HashMap<String, Live>>,
}

#[derive(Debug, Clone)]
struct Live {
    phase: CompactionPhase,
    /// When `phase` began.
    since: OffsetDateTime,
    /// The context when the compaction began.
    before: u64,
    /// The newest log read, and where reading stopped.
    log: Option<(PathBuf, UsageCursor)>,
    context: Option<u64>,
    model: Option<String>,
    measured_at: Option<String>,
}

impl Default for Live {
    fn default() -> Self {
        Self {
            phase: CompactionPhase::Idle,
            since: OffsetDateTime::UNIX_EPOCH,
            before: 0,
            log: None,
            context: None,
            model: None,
            measured_at: None,
        }
    }
}

impl Default for Compactor {
    fn default() -> Self {
        Self::new(
            CompactionSettings {
                auto: true,
                at_tokens: COMPACT_AT_TOKENS,
            },
            SessionRoots::default(),
        )
    }
}

impl Compactor {
    pub fn new(default: CompactionSettings, roots: SessionRoots) -> Self {
        Self {
            default,
            roots,
            live: Mutex::new(HashMap::new()),
        }
    }

    /// The default threshold from `QUARK_COORDINATOR_COMPACT_AT_TOKENS`
    /// (or the older `QUARK_COORDINATOR_WRAP_UP_TOKENS`), else
    /// [`COMPACT_AT_TOKENS`]; `0` turns automatic compaction off by default.
    pub fn from_env(roots: SessionRoots) -> Self {
        let at = [
            "QUARK_COORDINATOR_COMPACT_AT_TOKENS",
            "QUARK_COORDINATOR_WRAP_UP_TOKENS",
        ]
        .iter()
        .find_map(|v| std::env::var(v).ok()?.trim().parse::<u64>().ok());
        let default = match at {
            Some(0) => CompactionSettings {
                auto: false,
                at_tokens: COMPACT_AT_TOKENS,
            },
            Some(at_tokens) => CompactionSettings {
                auto: true,
                at_tokens,
            },
            None => CompactionSettings {
                auto: true,
                at_tokens: COMPACT_AT_TOKENS,
            },
        };
        Self::new(default, roots)
    }

    /// The settings of a Project that never set its own.
    pub fn defaults(&self) -> CompactionSettings {
        self.default
    }

    /// Record a coordinator turn that ended with `context` tokens (when
    /// known), and say what follows it.
    pub fn turn_ended(
        &self,
        project: &str,
        context: Option<u64>,
        settings: CompactionSettings,
    ) -> Step {
        self.turn_ended_at(project, context, settings, OffsetDateTime::now_utc())
    }

    fn turn_ended_at(
        &self,
        project: &str,
        context: Option<u64>,
        settings: CompactionSettings,
        now: OffsetDateTime,
    ) -> Step {
        let mut live = self.live.lock().unwrap();
        let l = live.entry(project.to_string()).or_default();
        if let Some(c) = context {
            l.context = Some(c);
            l.measured_at = now.format(&Rfc3339).ok();
        }
        match l.phase {
            // The wrap up's turn: compact now, even if it was turned off
            // meanwhile, since the coordinator was told it would be.
            CompactionPhase::WrappingUp => {
                l.phase = CompactionPhase::Compacting;
                l.since = now;
                return Step::Compact;
            }
            CompactionPhase::Compacting => {
                let shrank = context.is_some_and(|c| c < l.before);
                if !shrank && now - l.since < COMPACT_TIMEOUT {
                    return Step::Nothing;
                }
                l.phase = CompactionPhase::Idle;
                if shrank {
                    return Step::Nothing;
                }
            }
            CompactionPhase::Idle => {}
        }
        match context {
            Some(tokens)
                if settings.auto && settings.at_tokens > 0 && tokens >= settings.at_tokens =>
            {
                l.phase = CompactionPhase::WrappingUp;
                l.since = now;
                l.before = tokens;
                Step::WrapUp { tokens }
            }
            _ => Step::Nothing,
        }
    }

    /// The user asked to compact now: no wrap up first.
    fn compact_asked(&self, project: &str) {
        let mut live = self.live.lock().unwrap();
        let l = live.entry(project.to_string()).or_default();
        l.phase = CompactionPhase::Compacting;
        l.since = OffsetDateTime::now_utc();
        l.before = l.context.unwrap_or(u64::MAX);
    }

    /// A step could not be taken: nothing is under way.
    fn failed(&self, project: &str) {
        if let Some(l) = self.live.lock().unwrap().get_mut(project) {
            l.phase = CompactionPhase::Idle;
        }
    }

    /// The coordinator's agent, context and compaction, reading what its
    /// newest session log added since the last call. Blocking.
    pub fn context(
        &self,
        store: &Store,
        project: &Project,
    ) -> crate::store::Result<CoordinatorContext> {
        let compaction = store.compaction_settings(&project.id, self.default)?;
        let last_compaction = store.last_coordinator_compaction(&project.id)?;
        let mut read = self
            .live
            .lock()
            .unwrap()
            .get(&project.id)
            .cloned()
            .unwrap_or_default();
        if let Some(root) = &project.workspace_path {
            read_log(&self.roots, Path::new(root), &mut read);
        }
        let compacted_at = last_compaction
            .as_ref()
            .and_then(|c| c.at.as_deref())
            .and_then(|t| OffsetDateTime::parse(t, &Rfc3339).ok());
        let mut live = self.live.lock().unwrap();
        let l = live.entry(project.id.clone()).or_default();
        l.log = read.log;
        if read.measured_at >= l.measured_at {
            (l.context, l.model, l.measured_at) = (read.context, read.model, read.measured_at);
        }
        // A compaction the log recorded after Quark started one ends it.
        if l.phase == CompactionPhase::Compacting && compacted_at.is_some_and(|t| t >= l.since) {
            l.phase = CompactionPhase::Idle;
        }
        // Compacted after the last turn ended: its size is not known again
        // until the next one ends.
        let measured = l
            .measured_at
            .as_deref()
            .and_then(|t| OffsetDateTime::parse(t, &Rfc3339).ok());
        let context = match (compacted_at, measured) {
            (Some(c), Some(m)) if c > m => None,
            _ => l.context,
        };
        let agent = project.agent_config.clone();
        let window = window_tokens(agent.as_ref(), l.model.as_deref(), context);
        let model_seen = l
            .model
            .clone()
            .filter(|m| agent.as_ref().and_then(|a| a.model.as_deref()) != Some(m.as_str()));
        Ok(CoordinatorContext {
            coordinator_id: project.id.clone(),
            agent_config: agent,
            model_seen,
            context_tokens: context,
            window_tokens: window,
            measured_at: l.measured_at.clone(),
            compaction,
            compacting: l.phase,
            last_compaction,
        })
    }
}

/// Read the turns `l`'s log gained since it was last read, keeping the
/// context and model of the last one. A newer log is read from its start.
fn read_log(roots: &SessionRoots, root: &Path, l: &mut Live) {
    let Some((path, format)) = newest_log(roots, root) else {
        return;
    };
    let cursor = match &l.log {
        Some((p, c)) if *p == path => c.clone(),
        _ => UsageCursor::default(),
    };
    let Ok((turns, next)) = read_turns(&path, &cursor, format, true) else {
        return;
    };
    if let Some(t) = turns.iter().rev().find(|t| t.context.is_some()) {
        l.context = t.context;
        l.model = t
            .models
            .iter()
            .rev()
            .map(|m| m.model.clone())
            .find(|m| !m.is_empty())
            .or(l.model.take());
        l.measured_at = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| OffsetDateTime::from(t).format(&Rfc3339).ok());
    }
    l.log = Some((path, next));
}

/// The newest coordinator session log of the workspace at `root`.
pub fn newest_log(roots: &SessionRoots, root: &Path) -> Option<(PathBuf, SessionFormat)> {
    SessionFormat::COORDINATOR
        .iter()
        .filter_map(|&f| locate(f, root, roots).map(|p| (p, f)))
        .max_by_key(|(p, _)| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        })
}

/// The context window of the coordinator's model, when Quark knows it: 1M
/// for a Claude model asked for with `[1m]` (or seen holding more than
/// 200k), 200k for other Claude models, 272k for Codex's GPT-5 models.
pub fn window_tokens(
    agent: Option<&AgentConfig>,
    seen: Option<&str>,
    context: Option<u64>,
) -> Option<u64> {
    let configured = agent.and_then(|a| a.model.as_deref()).unwrap_or("");
    let harness = agent.map(|a| a.harness.as_str()).unwrap_or("");
    let seen = seen.unwrap_or("");
    let lower = |s: &str| s.to_ascii_lowercase();
    let (configured, seen) = (lower(configured), lower(seen));
    let any = |f: &dyn Fn(&str) -> bool| f(&configured) || f(&seen);
    let claude = harness == "claude-code"
        || harness == "claude"
        || any(&|m: &str| {
            ["claude", "opus", "sonnet", "haiku", "fable"]
                .iter()
                .any(|k| m.contains(k))
        });
    let window = if claude {
        if any(&|m: &str| m.contains("[1m]")) {
            1_000_000
        } else {
            200_000
        }
    } else if any(&|m: &str| m.starts_with("gpt-5")) || harness == "codex" {
        272_000
    } else {
        return None;
    };
    match context {
        Some(c) if c > window && claude => Some(1_000_000),
        Some(c) if c > window => None,
        _ => Some(window),
    }
}

/// After a coordinator turn that ended with `context` tokens, take the
/// compaction step that follows, in the background.
pub fn after_turn(
    compactor: Arc<Compactor>,
    store: Arc<Store>,
    events: quark_eventlog::SqliteEventLog,
    chat: Arc<dyn CoordinatorInput>,
    project: String,
    context: Option<u64>,
) {
    tokio::spawn(async move {
        let s = store.clone();
        let (id, default) = (project.clone(), compactor.defaults());
        let settings =
            match tokio::task::spawn_blocking(move || s.compaction_settings(&id, default)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    tracing::warn!(%project, error = %e, "could not read compaction settings");
                    return;
                }
                Err(_) => return,
            };
        match compactor.turn_ended(&project, context, settings) {
            Step::Nothing => {}
            Step::WrapUp { tokens } => {
                let ran = coordinator_commands::run(
                    &store,
                    &events,
                    chat.as_ref(),
                    &project,
                    quark_systems::CoordinatorCommand::WrapUp,
                    Asked::ContextFull { tokens },
                )
                .await;
                match ran {
                    Ok(_) => {
                        tracing::info!(%project, tokens, "asked the coordinator to wrap up before compacting")
                    }
                    Err(e) => {
                        compactor.failed(&project);
                        tracing::warn!(%project, error = %e, "could not ask the coordinator to wrap up");
                    }
                }
            }
            Step::Compact => match compact(&store, chat.as_ref(), &project).await {
                Ok(_) => tracing::info!(%project, "asked the coordinator's harness to compact"),
                Err(e) => {
                    compactor.failed(&project);
                    tracing::warn!(%project, error = %e, "could not compact the coordinator");
                }
            },
        }
    });
}

/// Compact the coordinator now, without a wrap up first. Types `/compact`
/// in the background, since its harness takes a while to record it.
pub async fn compact_now(
    compactor: Arc<Compactor>,
    store: Arc<Store>,
    chat: Arc<dyn CoordinatorInput>,
    project: String,
) -> Result<(), CommandError> {
    let s = store.clone();
    let id = project.clone();
    let p = tokio::task::spawn_blocking(move || s.get_project(&id))
        .await
        .map_err(|e| ChatError::Failed(e.to_string()))??;
    if p.workspace_path.is_none() {
        return Err(CommandError::NoWorkspace);
    }
    compactor.compact_asked(&project);
    tokio::spawn(async move {
        if let Err(e) = compact(&store, chat.as_ref(), &project).await {
            compactor.failed(&project);
            tracing::warn!(%project, error = %e, "could not compact the coordinator");
        }
    });
    Ok(())
}

/// Type `/compact` into the coordinator's session.
async fn compact(
    store: &Arc<Store>,
    chat: &dyn CoordinatorInput,
    project: &str,
) -> Result<Delivery, CommandError> {
    let s = store.clone();
    let id = project.to_string();
    let p = tokio::task::spawn_blocking(move || s.get_project(&id))
        .await
        .map_err(|e| ChatError::Failed(e.to_string()))??;
    let Some(root) = p.workspace_path else {
        return Err(CommandError::NoWorkspace);
    };
    let ws = WorkspaceRef {
        project_id: p.id,
        root: PathBuf::from(root),
    };
    Ok(chat.send(&ws, &format!("/compact {KEEP}")).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON: CompactionSettings = CompactionSettings {
        auto: true,
        at_tokens: 100,
    };

    #[test]
    fn a_full_turn_wraps_up_then_compacts_then_waits_for_it_to_shrink() {
        let c = Compactor::default();
        let t0 = OffsetDateTime::now_utc();
        assert_eq!(c.turn_ended_at("p", Some(90), ON, t0), Step::Nothing);
        assert_eq!(
            c.turn_ended_at("p", Some(120), ON, t0),
            Step::WrapUp { tokens: 120 }
        );
        // The wrap up's own turn.
        assert_eq!(c.turn_ended_at("p", Some(130), ON, t0), Step::Compact);
        // Still full: the compaction has not shown yet.
        assert_eq!(c.turn_ended_at("p", Some(135), ON, t0), Step::Nothing);
        // Smaller: compacted, and nothing more until it fills again.
        assert_eq!(c.turn_ended_at("p", Some(20), ON, t0), Step::Nothing);
        assert_eq!(c.turn_ended_at("p", Some(60), ON, t0), Step::Nothing);
        assert_eq!(
            c.turn_ended_at("p", Some(101), ON, t0),
            Step::WrapUp { tokens: 101 }
        );
    }

    #[test]
    fn a_compaction_never_seen_is_given_up_on() {
        let c = Compactor::default();
        let t0 = OffsetDateTime::now_utc();
        c.turn_ended_at("p", Some(120), ON, t0);
        c.turn_ended_at("p", Some(120), ON, t0);
        let later = t0 + COMPACT_TIMEOUT + time::Duration::seconds(1);
        assert_eq!(
            c.turn_ended_at("p", Some(125), ON, later),
            Step::WrapUp { tokens: 125 }
        );
    }

    #[test]
    fn off_or_unknown_never_compacts() {
        let c = Compactor::default();
        let off = CompactionSettings { auto: false, ..ON };
        assert_eq!(c.turn_ended("p", Some(1_000_000), off), Step::Nothing);
        assert_eq!(c.turn_ended("p", None, ON), Step::Nothing);
        let zero = CompactionSettings { at_tokens: 0, ..ON };
        assert_eq!(c.turn_ended("p", Some(1_000_000), zero), Step::Nothing);
    }

    #[test]
    fn the_window_follows_the_model() {
        let agent = |harness: &str, model: Option<&str>| AgentConfig {
            harness: harness.into(),
            model: model.map(Into::into),
            effort: None,
            pool: None,
        };
        let a = agent("claude-code", None);
        assert_eq!(window_tokens(Some(&a), None, Some(50_000)), Some(200_000));
        assert_eq!(
            window_tokens(Some(&a), None, Some(400_000)),
            Some(1_000_000)
        );
        let a = agent("claude-code", Some("opus[1m]"));
        assert_eq!(
            window_tokens(Some(&a), Some("claude-opus-5-5"), None),
            Some(1_000_000)
        );
        let a = agent("codex", Some("gpt-5-codex"));
        assert_eq!(window_tokens(Some(&a), None, None), Some(272_000));
        let a = agent("pi", Some("kimi-k2"));
        assert_eq!(window_tokens(Some(&a), None, None), None);
        assert_eq!(
            window_tokens(None, Some("claude-sonnet-5-5"), None),
            Some(200_000)
        );
    }
}
