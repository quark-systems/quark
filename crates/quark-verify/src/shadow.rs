//! Slice 2 in shadow mode: firstmate decides and acts; native checks every
//! decision and records where it would have decided differently.
//!
//! firstmate appends each guardrail decision to
//! `state/guard-decisions.jsonl` (schema `fm.guard.v1`, one [`GuardRecord`]
//! per line) with the inputs it decided on. For each new line the shadow
//! makes two comparisons:
//!
//! 1. **Rules.** Native's [`crate::rules`] run on the recorded inputs must
//!    reach the same permit and reason. A difference here is a rule
//!    difference (operation `may_merge` or `may_dispatch`).
//! 2. **Reads.** Native re-reads, through its own [`Forge`], the facts that
//!    are fixed once known: whether the recorded head contains the recorded
//!    tip (`head_fresh`), the checks on the recorded main tip
//!    (`main_health`), and which of main's failing checks passed on the
//!    pull request's head (`pr_green`). A read that fails is counted as
//!    unverified, not as a divergence.
//!
//! Every line yields one [`kinds::SHADOW`] event, and each disagreement one
//! [`quark_core::event::kinds::SHADOW_DIVERGENCE`] event for
//! [`Slice::Verification`]. Event ids derive from the line's file identity
//! and offset, and the read position is checkpointed in the same batch as
//! the events, so a line is compared once however the daemon stops.

use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use quark_core::event::kinds as core_kinds;
use quark_core::slice::Divergence;
use quark_core::{CoreError, EventId, HostId, NewEvent, ProjectId, Result, Slice, TaskId};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::forge::Forge;
use crate::health::{classify, green_checks, HealthState};
use crate::kinds;
use crate::rules::{
    dispatch_decision, merge_decision, Decision, DispatchFacts, Freshness, GateEvidence,
    MainStatus, MergeFacts, Reason,
};

/// The file firstmate appends decisions to, under its `state/`.
pub const DECISIONS_FILE: &str = "guard-decisions.jsonl";

/// Lines compared per pass, so one pass never makes an unbounded number of
/// forge reads.
pub const MAX_LINES_PER_PASS: usize = 200;

/// The event log plus a checkpoint written atomically with a batch: what the
/// shadow needs to compare each line exactly once.
#[async_trait]
pub trait CheckpointLog: Send + Sync {
    async fn checkpoint(&self, name: &str) -> Result<Option<String>>;

    /// Append `events` and set `checkpoint` in one transaction.
    async fn append_batch(
        &self,
        events: Vec<NewEvent>,
        checkpoint: Option<(String, String)>,
    ) -> Result<()>;
}

/// One `fm.guard.v1` line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardRecord {
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub ts: String,
    /// `merge` or `dispatch`.
    pub op: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub pr: Option<String>,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub head: Option<String>,
    /// `fresh`, `stale` or `unreadable`.
    #[serde(default)]
    pub fresh: Option<String>,
    /// The base tip the freshness read compared against.
    #[serde(default)]
    pub tip: Option<String>,
    #[serde(default)]
    pub behind: Option<u64>,
    #[serde(default)]
    pub gates: Option<RecordGates>,
    #[serde(default)]
    pub main: Option<RecordMain>,
    #[serde(default)]
    pub pr_green: Vec<String>,
    #[serde(default)]
    pub fix_main: Option<bool>,
    #[serde(default)]
    pub prior_fix_task: Option<String>,
    #[serde(default)]
    pub prior_fix_live: Option<bool>,
    /// `allow` or `deny`.
    pub permit: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordGates {
    pub required: bool,
    #[serde(default)]
    pub evidence_head: String,
    #[serde(default)]
    pub evidence_state: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordMain {
    /// `clear`, `red`, `overridden` or `unknown`.
    pub status: String,
    /// The raw tip read: `green`, `red`, `pending`, `none`, or empty.
    #[serde(default)]
    pub read: String,
    #[serde(default)]
    pub tip: String,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub decision: String,
}

/// Payload of a [`kinds::SHADOW`] event: one firstmate decision checked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowCheck {
    pub op: String,
    pub repo: String,
    pub bash: Verdict,
    /// Native's answer on the recorded inputs; `None` when they can't be
    /// replayed (firstmate failed to write its own record).
    pub native: Option<Verdict>,
    /// The comparisons made, such as `may_merge`, `head_fresh`.
    pub compared: Vec<String>,
    /// Comparisons whose native read failed.
    pub unverified: Vec<String>,
    /// Comparisons that disagreed.
    pub diverged: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub allow: bool,
    pub reason: String,
}

impl From<&Decision> for Verdict {
    fn from(d: &Decision) -> Self {
        Self {
            allow: d.allow,
            reason: d.reason.as_str().into(),
        }
    }
}

/// What one pass compared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShadowReport {
    pub decisions: usize,
    pub divergences: usize,
    pub unparsed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    dev: u64,
    ino: u64,
    offset: u64,
}

/// Compares firstmate's guard decisions with native's.
pub struct ShadowVerifier {
    log: Arc<dyn CheckpointLog>,
    forge: Option<Arc<dyn Forge>>,
    host: HostId,
}

impl ShadowVerifier {
    /// Without a forge, only the rules comparison runs.
    pub fn new(log: Arc<dyn CheckpointLog>, forge: Option<Arc<dyn Forge>>, host: HostId) -> Self {
        Self { log, forge, host }
    }

    /// Compare every new decision in the firstmate home `home`, which belongs
    /// to `project`. Safe to call again at any time.
    pub async fn ingest(&self, project: &ProjectId, home: &Path) -> Result<ShadowReport> {
        let path = home.join("state").join(DECISIONS_FILE);
        let name = format!("verify-shadow/{project}");
        let saved: Option<Cursor> = self
            .log
            .checkpoint(&name)
            .await?
            .and_then(|v| serde_json::from_str(&v).ok());
        let path_buf = path.clone();
        let read = tokio::task::spawn_blocking(move || read_lines(&path_buf, saved))
            .await
            .map_err(|e| CoreError::Backend(e.to_string()))??;
        let Some((cursor, lines)) = read else {
            return Ok(ShadowReport::default());
        };
        if saved == Some(cursor) && lines.is_empty() {
            return Ok(ShadowReport::default());
        }
        let mut report = ShadowReport::default();
        let mut events = Vec::new();
        for (offset, line) in &lines {
            let seed = format!("{project}|{}|{}|{offset}", cursor.dev, cursor.ino);
            let record: GuardRecord = match serde_json::from_str(line) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(path = %path.display(), offset, error = %e, "unreadable guard decision");
                    report.unparsed += 1;
                    continue;
                }
            };
            report.decisions += 1;
            let (check, divergences) = self.compare(&record).await;
            report.divergences += divergences.len();
            let task = (!record.task.is_empty()).then(|| TaskId::new(record.task.clone()));
            let mut n = 0;
            let mut push = |kind: &str, payload: serde_json::Value| -> Result<()> {
                let mut e = NewEvent::new(
                    self.host.clone(),
                    project.clone(),
                    task.clone(),
                    kind,
                    payload,
                );
                e.id = event_id(&seed, n);
                n += 1;
                events.push(e);
                Ok(())
            };
            push(kinds::SHADOW, to_value(&check)?)?;
            for d in &divergences {
                push(core_kinds::SHADOW_DIVERGENCE, to_value(d)?)?;
            }
        }
        let value =
            serde_json::to_string(&cursor).map_err(|e| CoreError::Backend(e.to_string()))?;
        self.log.append_batch(events, Some((name, value))).await?;
        Ok(report)
    }

    /// Compare one record: the rules on its inputs, then native reads.
    pub async fn compare(&self, r: &GuardRecord) -> (ShadowCheck, Vec<Divergence>) {
        let bash = Verdict {
            allow: r.permit == "allow",
            reason: r.reason.clone(),
        };
        let mut check = ShadowCheck {
            op: r.op.clone(),
            repo: r.repo.clone(),
            bash: bash.clone(),
            native: None,
            compared: Vec::new(),
            unverified: Vec::new(),
            diverged: Vec::new(),
        };
        let mut divergences = Vec::new();
        let mut diverge = |check: &mut ShadowCheck,
                           op: &str,
                           bash: serde_json::Value,
                           native: serde_json::Value| {
            check.diverged.push(op.to_string());
            divergences.push(Divergence {
                slice: Slice::Verification,
                operation: op.to_string(),
                bash,
                native,
            });
        };

        let operation = match r.op.as_str() {
            "merge" => "may_merge",
            "dispatch" => "may_dispatch",
            _ => "unknown_op",
        };
        if let Some(native) = replay(r) {
            let native = Verdict::from(&native);
            check.compared.push(operation.into());
            if native != bash {
                diverge(&mut check, operation, json!(bash), json!(native));
            }
            check.native = Some(native);
        }

        let Some(forge) = &self.forge else {
            return (check, divergences);
        };

        // Freshness: does the recorded head contain the recorded tip?
        if r.op == "merge" {
            if let (Some(tip), Some(head), Some(fresh)) = (&r.tip, &r.head, &r.fresh) {
                if matches!(fresh.as_str(), "fresh" | "stale") && !tip.is_empty() {
                    check.compared.push("head_fresh".into());
                    match forge.behind_by(&r.repo, tip, head).await {
                        Ok(behind) => {
                            let native = Freshness::from_behind(Some(behind));
                            let recorded = if fresh == "fresh" {
                                Freshness::Fresh
                            } else {
                                Freshness::Stale
                            };
                            if native != recorded {
                                diverge(
                                    &mut check,
                                    "head_fresh",
                                    json!({"fresh": fresh, "behind": r.behind}),
                                    json!({"fresh": native, "behind": behind}),
                                );
                            }
                        }
                        Err(_) => check.unverified.push("head_fresh".into()),
                    }
                }
            }
        }

        // Main's health on the tip firstmate read.
        if let Some(main) = &r.main {
            if let Some(state) = HealthState::parse(&main.read).filter(|_| !main.tip.is_empty()) {
                check.compared.push("main_health".into());
                match forge.checks(&r.repo, &main.tip).await {
                    Ok((runs, statuses)) => {
                        let native = classify(&main.tip, &runs, &statuses);
                        // firstmate keeps the episode's checks, which are the
                        // read's failing checks whenever the read is red.
                        let differs = native.state != state
                            || (state == HealthState::Red && native.red != main.checks);
                        if differs {
                            diverge(
                                &mut check,
                                "main_health",
                                json!({"state": main.read, "red": if state == HealthState::Red { main.checks.clone() } else { vec![] }}),
                                json!({"state": native.state, "red": native.red}),
                            );
                        }
                    }
                    Err(_) => check.unverified.push("main_health".into()),
                }
            }

            // Which of main's failing checks the pull request passed.
            if r.op == "merge" && main.status == "red" && !main.checks.is_empty() {
                if let Some(head) = &r.head {
                    check.compared.push("pr_green".into());
                    match forge.checks(&r.repo, head).await {
                        Ok((runs, statuses)) => {
                            let native = green_checks(&runs, &statuses);
                            let failing: BTreeSet<&String> = main.checks.iter().collect();
                            let bash_green: BTreeSet<&String> =
                                r.pr_green.iter().filter(|c| failing.contains(c)).collect();
                            let native_green: BTreeSet<&String> =
                                native.iter().filter(|c| failing.contains(c)).collect();
                            if bash_green != native_green {
                                diverge(
                                    &mut check,
                                    "pr_green",
                                    json!(bash_green),
                                    json!(native_green),
                                );
                            }
                        }
                        Err(_) => check.unverified.push("pr_green".into()),
                    }
                }
            }
        }
        (check, divergences)
    }
}

/// Native's decision on a record's inputs; `None` when they can't be
/// replayed.
pub fn replay(r: &GuardRecord) -> Option<Decision> {
    if r.reason == Reason::RedMainRecordFailed.as_str() {
        return None;
    }
    let main = r.main.as_ref();
    let status = main
        .and_then(|m| MainStatus::parse(&m.status))
        .unwrap_or(MainStatus::Unknown);
    match r.op.as_str() {
        "merge" => {
            let freshness = match r.fresh.as_deref() {
                Some("fresh") => Freshness::Fresh,
                Some("stale") => Freshness::Stale,
                _ => Freshness::Unreadable,
            };
            let gates = r.gates.as_ref().filter(|g| g.required).map(|g| {
                (!g.evidence_head.is_empty() || !g.evidence_state.is_empty()).then(|| {
                    GateEvidence {
                        head: g.evidence_head.clone(),
                        state: g.evidence_state.clone(),
                    }
                })
            });
            Some(merge_decision(&MergeFacts {
                head: r.head.clone().unwrap_or_default(),
                freshness,
                gates,
                main: status,
                failing: main.map(|m| m.checks.clone()).unwrap_or_default(),
                green: r.pr_green.iter().cloned().collect(),
            }))
        }
        "dispatch" => Some(dispatch_decision(&DispatchFacts {
            task: r.task.clone(),
            main: status,
            fix_main: r.fix_main.unwrap_or(false),
            prior_fix: r.prior_fix_task.clone().filter(|t| !t.is_empty()),
            prior_fix_live: r.prior_fix_live.unwrap_or(false),
        })),
        _ => None,
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<serde_json::Value> {
    serde_json::to_value(v).map_err(|e| CoreError::Invalid(e.to_string()))
}

/// A stable id for the `n`th event made from one line.
fn event_id(seed: &str, n: usize) -> EventId {
    const NAMESPACE: uuid::Uuid = uuid::Uuid::from_u128(0x6a1f_3c0e_92d4_4b7a_9e55_0c2d_7f31_a8b4);
    EventId(uuid::Uuid::new_v5(
        &NAMESPACE,
        format!("{seed}|{n}").as_bytes(),
    ))
}

/// Complete lines after the saved offset, with their offsets, and the cursor
/// after them. A replaced or truncated file is read from the start. `None`
/// when there is no file.
/// Lines with their byte offsets.
type Lines = Vec<(u64, String)>;

fn read_lines(path: &Path, saved: Option<Cursor>) -> Result<Option<(Cursor, Lines)>> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};
    let io = |e: std::io::Error| CoreError::Backend(format!("{}: {e}", path.display()));
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
    };
    let meta = file.metadata().map_err(io)?;
    let (dev, ino) = (meta.dev(), meta.ino());
    let mut offset = saved
        .filter(|c| c.dev == dev && c.ino == ino && c.offset <= meta.len())
        .map_or(0, |c| c.offset);
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(offset)).map_err(io)?;
    let mut lines = Vec::new();
    let mut buf = Vec::new();
    while lines.len() < MAX_LINES_PER_PASS {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf).map_err(io)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            // End of file, or a line still being written.
            break;
        }
        let text = String::from_utf8_lossy(&buf[..n - 1]).trim().to_string();
        if !text.is_empty() {
            lines.push((offset, text));
        }
        offset += n as u64;
    }
    Ok(Some((Cursor { dev, ino, offset }, lines)))
}

/// A [`CheckpointLog`] in memory, over a [`quark_core::fake::MemoryEventLog`].
#[derive(Default)]
pub struct MemoryCheckpointLog {
    pub log: quark_core::fake::MemoryEventLog,
    checkpoints: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl MemoryCheckpointLog {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CheckpointLog for MemoryCheckpointLog {
    async fn checkpoint(&self, name: &str) -> Result<Option<String>> {
        Ok(self.checkpoints.lock().unwrap().get(name).cloned())
    }

    async fn append_batch(
        &self,
        events: Vec<NewEvent>,
        checkpoint: Option<(String, String)>,
    ) -> Result<()> {
        use quark_core::EventLog;
        for e in events {
            self.log.append(e).await?;
        }
        if let Some((k, v)) = checkpoint {
            self.checkpoints.lock().unwrap().insert(k, v);
        }
        Ok(())
    }
}
