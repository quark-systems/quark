//! The away policy: what reaches the coordinator and the user, by posture.
//!
//! The user is `present`, `away` (gone for a while; user-owned decisions
//! wait for their return) or `quiet` (here, but only wants what matters).
//! A posture is set globally or per Project; a Project's own setting wins.
//!
//! Every engine occasion (a worker finished, asked, got blocked, failed,
//! went quiet; a message arrived; a trigger fired) gets a [`Route`]: whether
//! the coordinator is woken for judgment, and whether the user hears now,
//! in the next digest, when they return, or not at all. Routine progress
//! never wakes anyone, which is where the coordinator's token savings come
//! from. The defaults reproduce firstmate's away daemon; a Project can
//! override any cell.

use std::collections::BTreeMap;

use quark_core::{Event, EventKind, ProjectId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The prefix every away event kind starts with.
pub const PREFIX: &str = "away";

/// Where the user is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Posture {
    #[default]
    Present,
    /// Gone; user-owned decisions hold for their return.
    Away,
    /// Present but only wants what matters; nothing holds.
    Quiet,
}

impl Posture {
    pub fn as_str(self) -> &'static str {
        match self {
            Posture::Present => "present",
            Posture::Away => "away",
            Posture::Quiet => "quiet",
        }
    }
}

/// What happened, as far as routing cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Occasion {
    /// Routine progress (`working`, a decision resolved).
    Progress,
    /// A declared external wait.
    Paused,
    /// The deliverable is ready.
    Done,
    /// A worker asks a question.
    Decision,
    Blocked,
    Failed,
    /// A worker has shown no activity for a long time.
    Stale,
    /// A message arrived on a channel.
    Inbound,
    /// A trigger's action ran.
    TriggerFired,
    /// A trigger's condition or action failed, or its outcome is unknown.
    TriggerFailed,
}

impl Occasion {
    pub const ALL: [Occasion; 10] = [
        Occasion::Progress,
        Occasion::Paused,
        Occasion::Done,
        Occasion::Decision,
        Occasion::Blocked,
        Occasion::Failed,
        Occasion::Stale,
        Occasion::Inbound,
        Occasion::TriggerFired,
        Occasion::TriggerFailed,
    ];
}

/// How the user hears about an occasion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    /// Not at all.
    Silent,
    /// In the next digest.
    Digest,
    /// Right away.
    Notify,
    /// Kept for the user's return; anything that needs their word waits.
    Hold,
}

/// Where one occasion goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Route {
    /// Wake the coordinator for judgment.
    pub wake: bool,
    pub user: Reach,
}

impl Route {
    pub const fn new(wake: bool, user: Reach) -> Self {
        Self { wake, user }
    }
}

/// The routes for each posture, plus the digest cadence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwayPolicy {
    /// Overrides of the default table, by posture and occasion.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub routes: BTreeMap<String, BTreeMap<Occasion, Route>>,
    /// Seconds between digests while away or quiet.
    #[serde(default = "default_digest_secs")]
    pub digest_secs: u64,
}

fn default_digest_secs() -> u64 {
    // firstmate batches away escalations for 90s; a digest for a person
    // reads better hourly.
    3600
}

impl Default for AwayPolicy {
    fn default() -> Self {
        Self {
            routes: BTreeMap::new(),
            digest_secs: default_digest_secs(),
        }
    }
}

impl AwayPolicy {
    /// The route for `occasion` in `posture`: an override, else the default.
    pub fn route(&self, posture: Posture, occasion: Occasion) -> Route {
        self.routes
            .get(posture.as_str())
            .and_then(|r| r.get(&occasion))
            .copied()
            .unwrap_or_else(|| default_route(posture, occasion))
    }

    /// Override one cell.
    pub fn set(&mut self, posture: Posture, occasion: Occasion, route: Route) {
        self.routes
            .entry(posture.as_str().to_string())
            .or_default()
            .insert(occasion, route);
    }

    /// Refuse a policy that would lose a decision: a worker's question must
    /// reach the coordinator or the user in every posture.
    pub fn validate(&self) -> quark_core::Result<()> {
        if self.digest_secs == 0 {
            return Err(quark_core::CoreError::Invalid(
                "digest interval must be at least one second".into(),
            ));
        }
        for posture in [Posture::Present, Posture::Away, Posture::Quiet] {
            for occasion in [
                Occasion::Decision,
                Occasion::Failed,
                Occasion::TriggerFailed,
            ] {
                let r = self.route(posture, occasion);
                if !r.wake && r.user == Reach::Silent {
                    return Err(quark_core::CoreError::Invalid(format!(
                        "{occasion:?} while {} would reach no one",
                        posture.as_str()
                    )));
                }
            }
        }
        Ok(())
    }
}

/// The built-in table. The coordinator column matches firstmate's away
/// daemon (only done, decision, blocked and failed escalate; a stale worker
/// is checked by the engine first); the user column is new.
pub fn default_route(posture: Posture, occasion: Occasion) -> Route {
    use Occasion::*;
    use Reach::*;
    let wake = matches!(
        occasion,
        Done | Decision | Blocked | Failed | Inbound | TriggerFailed
    );
    let user = match (posture, occasion) {
        (_, Progress | Paused | Stale | Inbound) => Silent,
        (Posture::Present, Done | Decision | Blocked | Failed | TriggerFailed) => Notify,
        (Posture::Present, TriggerFired) => Digest,
        (Posture::Away, Decision | Blocked) => Hold,
        (Posture::Away, Done | Failed | TriggerFired | TriggerFailed) => Digest,
        (Posture::Quiet, Decision | Failed | TriggerFailed) => Notify,
        (Posture::Quiet, Done | Blocked | TriggerFired) => Digest,
    };
    Route::new(wake, user)
}

/// Payload of every `away.*` event; the kind is `away.<type>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AwayEvent {
    /// The user's posture changed. On the engine project it is global.
    Posture {
        posture: Posture,
        /// The user's own words when they left, kept verbatim.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        words: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "time::serde::rfc3339::option"
        )]
        expected_return: Option<OffsetDateTime>,
        by: String,
    },
    /// The policy changed. On the engine project it is global.
    Policy { policy: AwayPolicy, by: String },
    /// Native mode routed an occasion (written only for routes that reach
    /// someone), so digests and the return brief survive a restart.
    Routed {
        /// The event that caused it.
        source: quark_core::Seq,
        occasion: Occasion,
        route: Route,
        summary: String,
    },
    /// A digest went out, covering routed events up to `through`.
    DigestSent {
        through: quark_core::Seq,
        items: usize,
        /// The return brief, which also carried the held items.
        #[serde(default)]
        returning: bool,
    },
    /// Shadow mode compared routes for the events up to `through`.
    Shadowed {
        through: quark_core::Seq,
        compared: usize,
        diverged: usize,
    },
}

impl AwayEvent {
    pub fn kind(&self) -> EventKind {
        let name = match self {
            AwayEvent::Posture { .. } => "posture",
            AwayEvent::Policy { .. } => "policy",
            AwayEvent::Routed { .. } => "routed",
            AwayEvent::DigestSent { .. } => "digest_sent",
            AwayEvent::Shadowed { .. } => "shadowed",
        };
        EventKind::new(format!("{PREFIX}.{name}"))
    }
}

/// One item waiting for a digest or the user's return.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub seq: quark_core::Seq,
    pub source: quark_core::Seq,
    pub task: Option<quark_core::TaskId>,
    pub occasion: Occasion,
    pub summary: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

#[derive(Debug, Clone, Default)]
struct ProjectAway {
    posture: Option<Posture>,
    policy: Option<AwayPolicy>,
    digest: Vec<Item>,
    held: Vec<Item>,
    last_digest: Option<OffsetDateTime>,
}

/// Postures, policies and waiting items, folded from `away.*` events.
#[derive(Debug, Clone, Default)]
pub struct AwayState {
    projects: BTreeMap<ProjectId, ProjectAway>,
    shadowed_through: quark_core::Seq,
}

impl AwayState {
    pub fn apply(&mut self, e: &Event) {
        if e.kind.prefix() != PREFIX {
            return;
        }
        let Ok(ev) = e.decode::<AwayEvent>() else {
            tracing::warn!(
                seq = e.seq.0,
                kind = e.kind.as_str(),
                "unreadable away event"
            );
            return;
        };
        let p = self.projects.entry(e.project.clone()).or_default();
        match ev {
            AwayEvent::Posture { posture, .. } => {
                p.posture = Some(posture);
            }
            AwayEvent::Policy { policy, .. } => p.policy = Some(policy),
            AwayEvent::Routed {
                source,
                occasion,
                route,
                summary,
            } => {
                let item = Item {
                    seq: e.seq,
                    source,
                    task: e.task.clone(),
                    occasion,
                    summary,
                    at: e.ts,
                };
                match route.user {
                    Reach::Digest => {
                        if !p.digest.iter().any(|i| i.seq == e.seq) {
                            p.digest.push(item)
                        }
                    }
                    Reach::Hold => {
                        if !p.held.iter().any(|i| i.seq == e.seq) {
                            p.held.push(item)
                        }
                    }
                    Reach::Silent | Reach::Notify => {}
                }
            }
            AwayEvent::DigestSent {
                through, returning, ..
            } => {
                p.digest.retain(|i| i.seq > through);
                if returning {
                    p.held.retain(|i| i.seq > through);
                }
                p.last_digest = Some(e.ts);
            }
            AwayEvent::Shadowed { through, .. } => {
                self.shadowed_through = self.shadowed_through.max(through);
            }
        }
    }

    /// The posture for `project`: its own, else the global one.
    pub fn posture(&self, project: &ProjectId) -> Posture {
        self.projects
            .get(project)
            .and_then(|p| p.posture)
            .or_else(|| {
                self.projects
                    .get(&ProjectId::engine())
                    .and_then(|p| p.posture)
            })
            .unwrap_or_default()
    }

    /// The posture `project` set itself, without the global fallback.
    pub fn own_posture(&self, project: &ProjectId) -> Option<Posture> {
        self.projects.get(project).and_then(|p| p.posture)
    }

    /// The policy for `project`: its own, else the global one, else the
    /// default.
    pub fn policy(&self, project: &ProjectId) -> AwayPolicy {
        self.projects
            .get(project)
            .and_then(|p| p.policy.clone())
            .or_else(|| {
                self.projects
                    .get(&ProjectId::engine())
                    .and_then(|p| p.policy.clone())
            })
            .unwrap_or_default()
    }

    pub fn route(&self, project: &ProjectId, occasion: Occasion) -> Route {
        self.policy(project).route(self.posture(project), occasion)
    }

    /// Items waiting for the next digest.
    pub fn digest(&self, project: &ProjectId) -> &[Item] {
        self.projects
            .get(project)
            .map(|p| p.digest.as_slice())
            .unwrap_or(&[])
    }

    /// Items held for the user's return.
    pub fn held(&self, project: &ProjectId) -> &[Item] {
        self.projects
            .get(project)
            .map(|p| p.held.as_slice())
            .unwrap_or(&[])
    }

    /// Whether `project`'s digest should go out at `now`.
    pub fn digest_due(&self, project: &ProjectId, now: OffsetDateTime) -> bool {
        let Some(p) = self.projects.get(project) else {
            return false;
        };
        let Some(first) = p.digest.first() else {
            return false;
        };
        let every = time::Duration::seconds(self.policy(project).digest_secs as i64);
        // A full interval after the first waiting item, and never sooner than
        // one interval after the last digest.
        let since = p.last_digest.map_or(first.at, |d| d.max(first.at));
        now - since >= every
    }

    /// Projects with their own posture, policy or waiting items.
    pub fn projects(&self) -> impl Iterator<Item = &ProjectId> {
        self.projects.keys()
    }

    /// How far shadow mode has compared.
    pub fn shadowed_through(&self) -> quark_core::Seq {
        self.shadowed_through
    }
}

#[cfg(test)]
mod tests {
    use quark_core::{HostId, NewEvent, Seq};

    use super::*;

    fn ev(project: ProjectId, seq: u64, a: &AwayEvent) -> Event {
        NewEvent::typed(HostId::from("h"), project, None, a.kind(), a)
            .unwrap()
            .with_seq(Seq(seq))
    }

    #[test]
    fn defaults_follow_firstmate_and_never_lose_a_decision() {
        let p = AwayPolicy::default();
        p.validate().unwrap();
        assert!(!p.route(Posture::Away, Occasion::Progress).wake);
        assert!(!p.route(Posture::Away, Occasion::Paused).wake);
        assert!(p.route(Posture::Away, Occasion::Done).wake);
        assert_eq!(p.route(Posture::Away, Occasion::Decision).user, Reach::Hold);
        assert_eq!(
            p.route(Posture::Quiet, Occasion::Decision).user,
            Reach::Notify
        );
        assert_eq!(
            p.route(Posture::Present, Occasion::Done).user,
            Reach::Notify
        );
        let mut bad = p.clone();
        bad.set(
            Posture::Away,
            Occasion::Decision,
            Route::new(false, Reach::Silent),
        );
        assert!(bad.validate().is_err());
    }

    #[test]
    fn project_posture_and_policy_override_global() {
        let mut s = AwayState::default();
        let p = ProjectId::from("p");
        s.apply(&ev(
            ProjectId::engine(),
            1,
            &AwayEvent::Posture {
                posture: Posture::Away,
                words: Some("back monday".into()),
                expected_return: None,
                by: "user".into(),
            },
        ));
        assert_eq!(s.posture(&p), Posture::Away);
        s.apply(&ev(
            p.clone(),
            2,
            &AwayEvent::Posture {
                posture: Posture::Quiet,
                words: None,
                expected_return: None,
                by: "user".into(),
            },
        ));
        assert_eq!(s.posture(&p), Posture::Quiet);
        assert_eq!(s.posture(&"other".into()), Posture::Away);

        let mut policy = AwayPolicy::default();
        policy.set(
            Posture::Quiet,
            Occasion::Done,
            Route::new(false, Reach::Notify),
        );
        s.apply(&ev(
            p.clone(),
            3,
            &AwayEvent::Policy {
                policy,
                by: "user".into(),
            },
        ));
        assert_eq!(
            s.route(&p, Occasion::Done),
            Route::new(false, Reach::Notify)
        );
        assert_eq!(
            s.route(&"other".into(), Occasion::Done),
            default_route(Posture::Away, Occasion::Done)
        );
    }

    #[test]
    fn digest_and_held_items() {
        let mut s = AwayState::default();
        let p = ProjectId::from("p");
        let routed = |seq, occasion, user| {
            ev(
                p.clone(),
                seq,
                &AwayEvent::Routed {
                    source: Seq(seq - 1),
                    occasion,
                    route: Route::new(true, user),
                    summary: format!("item {seq}"),
                },
            )
        };
        s.apply(&routed(2, Occasion::Done, Reach::Digest));
        s.apply(&routed(2, Occasion::Done, Reach::Digest));
        s.apply(&routed(4, Occasion::Decision, Reach::Hold));
        s.apply(&routed(6, Occasion::Failed, Reach::Digest));
        assert_eq!(s.digest(&p).len(), 2);
        assert_eq!(s.held(&p).len(), 1);
        s.apply(&ev(
            p.clone(),
            7,
            &AwayEvent::DigestSent {
                through: Seq(2),
                items: 1,
                returning: false,
            },
        ));
        assert_eq!(s.digest(&p).len(), 1);
        assert_eq!(s.digest(&p)[0].summary, "item 6");
        assert_eq!(s.held(&p).len(), 1);
        s.apply(&ev(
            p.clone(),
            8,
            &AwayEvent::DigestSent {
                through: Seq(6),
                items: 2,
                returning: true,
            },
        ));
        assert!(s.held(&p).is_empty());
        assert!(s.digest(&p).is_empty());
    }
}
