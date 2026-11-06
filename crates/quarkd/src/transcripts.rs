//! The transcript tap: projects harness session logs into
//! `coordinator.message` and `worker.transcript` events.
//!
//! A worker's log is found from its worktree and its harness; a coordinator's
//! from its Project's workspace root, trying every supported harness. An
//! agent running under an added account (ADR-11) writes its log under that
//! account's config directory, so those are searched too. A worker's
//! worktree may be a reused pool slot, so a log written before its task
//! took the slot is never read as its own. Only
//! byte offsets are stored (`transcript_index`), so deleting the database
//! re-reads every log from the start.
//!
//! IBM Bob keeps no log file: a Bob worker's conversation is the rows of
//! Bob's database (`~/.bob/db/bob.db`, table `messages`) for the tasks it ran
//! in its worktree, and its stored offset is the last row read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use quark_transcript::{locate_since, read_from, ApiError, RateLimit, SessionFormat, SessionRoots};

use crate::store::{Store, TranscriptSource};

/// How long a located log is trusted before looking again, so a relaunched
/// agent's new log is picked up without rescanning harness directories on
/// every tick.
const RELOCATE_AFTER: Duration = Duration::from_secs(10);

/// Most Bob message rows one [`TranscriptTap::poll_bob`] reads; a backlog is
/// projected over several ticks.
const BOB_BATCH: usize = 500;

type Located = Option<(PathBuf, SessionFormat)>;

/// What one [`TranscriptTap::poll`] read.
#[derive(Debug, Default)]
pub struct Polled {
    /// The log gained lines.
    pub moved: bool,
    /// The rate limit the new lines leave the agent stopped at.
    pub rate_limit: Option<RateLimit>,
    /// The passing API error the new lines leave the agent stopped at.
    pub api_error: Option<ApiError>,
    /// The refused sign-in the new lines leave the agent stopped at.
    pub auth_error: Option<ApiError>,
    /// The new lines include something the agent itself wrote.
    pub agent_wrote: bool,
}

pub struct TranscriptTap {
    store: Arc<Store>,
    roots: SessionRoots,
    /// Bob's database, `~/.bob/db/bob.db`.
    bob_db: PathBuf,
    located: Mutex<HashMap<TranscriptSource, (Instant, Located)>>,
    /// Held while a log is read and its entries appended, so two polls of
    /// the same source never append the same lines twice.
    polling: Mutex<()>,
}

impl TranscriptTap {
    pub fn new(store: Arc<Store>, roots: SessionRoots) -> Self {
        Self {
            store,
            roots,
            bob_db: std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".bob").join("db").join("bob.db"))
                .unwrap_or_default(),
            located: Mutex::new(HashMap::new()),
            polling: Mutex::new(()),
        }
    }

    /// Reads Bob's conversations from `db` instead of `~/.bob/db/bob.db`.
    pub fn with_bob_db(mut self, db: PathBuf) -> Self {
        self.bob_db = db;
        self
    }

    pub fn roots(&self) -> &SessionRoots {
        &self.roots
    }

    pub fn bob_db(&self) -> &Path {
        &self.bob_db
    }

    /// Like [`Self::poll`], for a Bob worker in `cwd`: appends the messages
    /// Bob wrote for its tasks there since the last call. `since` is when
    /// the worker's task took `cwd`. Blocking: call from a blocking task.
    pub fn poll_bob(
        &self,
        project_id: &str,
        source: TranscriptSource,
        cwd: &Path,
        since: Option<SystemTime>,
    ) -> anyhow::Result<Polled> {
        let db = self.bob_db.to_string_lossy().into_owned();
        let _polling = self.polling.lock().unwrap_or_else(|e| e.into_inner());
        let after = match self.store.transcript_cursor(&source)? {
            Some((p, offset)) if p == db => offset as i64,
            _ => 0,
        };
        let rows =
            quark_dispatch::local::bob::task_messages(&self.bob_db, cwd, since, after, BOB_BATCH)
                .map_err(|e| anyhow::anyhow!("reading {db}: {e}"))?;
        let Some(last) = rows.last().map(|m| m.seq) else {
            return Ok(Polled::default());
        };
        let workspace = cwd.to_string_lossy();
        let entries: Vec<_> = rows
            .iter()
            .flat_map(|m| {
                let ts = m.created_ms.and_then(rfc3339_ms);
                quark_transcript::parse_bob_message(&m.data, ts.as_deref(), Some(&workspace))
            })
            .collect();
        self.store
            .apply_transcript(project_id, &source, &db, last as u64, &entries)?;
        Ok(Polled {
            moved: true,
            ..Polled::default()
        })
    }

    /// Reads whatever `cwd`'s session log gained since the last call and
    /// appends it as events. Returns whether the log moved and the rate
    /// limit or passing API error the new lines leave the agent stopped at,
    /// if any; each is returned once, when its line is first read. `since`
    /// is when the agent's task took `cwd`, where known. Blocking: call from
    /// a blocking task.
    pub fn poll(
        &self,
        project_id: &str,
        source: TranscriptSource,
        cwd: &Path,
        formats: &[SessionFormat],
        since: Option<SystemTime>,
    ) -> anyhow::Result<Polled> {
        let Some((path, format)) = self.locate(&source, cwd, formats, since) else {
            return Ok(Polled::default());
        };
        let path_str = path.to_string_lossy().into_owned();
        let _polling = self.polling.lock().unwrap_or_else(|e| e.into_inner());
        let cursor = self.store.transcript_cursor(&source)?;
        let offset = match &cursor {
            Some((p, offset)) if *p == path_str => *offset,
            _ => 0,
        };
        let batch = match read_from(&path, offset, format) {
            Ok(b) => b,
            Err(e) => {
                // The log moved or vanished: look again next tick.
                self.located.lock().unwrap().remove(&source);
                return Err(anyhow::anyhow!("reading {}: {e}", path.display()));
            }
        };
        if batch.malformed > 0 {
            tracing::warn!(path = %path.display(), lines = batch.malformed, "skipped malformed session-log lines");
        }
        let moved = cursor.as_ref().map(|(p, o)| (p.as_str(), *o))
            != Some((path_str.as_str(), batch.next_offset));
        if !moved {
            return Ok(Polled::default());
        }
        {
            let entries: Vec<_> = batch.entries.into_iter().map(|(_, e)| e).collect();
            self.store.apply_transcript(
                project_id,
                &source,
                &path_str,
                batch.next_offset,
                &entries,
            )?;
        }
        if batch.rate_limit.is_some() {
            // The agent is about to be replaced: look for its new log.
            self.located.lock().unwrap().remove(&source);
        }
        Ok(Polled {
            moved: true,
            rate_limit: batch.rate_limit,
            api_error: batch.api_error,
            auth_error: batch.auth_error,
            agent_wrote: batch.agent_wrote,
        })
    }

    fn locate(
        &self,
        source: &TranscriptSource,
        cwd: &Path,
        formats: &[SessionFormat],
        since: Option<SystemTime>,
    ) -> Located {
        if let Some((at, found)) = self.located.lock().unwrap().get(source) {
            if at.elapsed() < RELOCATE_AFTER {
                return found.clone();
            }
        }
        let roots = roots_with_accounts(&self.store, &self.roots);
        let found = locate_any(cwd, formats, &roots, since);
        self.located
            .lock()
            .unwrap()
            .insert(source.clone(), (Instant::now(), found.clone()));
        found
    }
}

/// Epoch milliseconds as an RFC 3339 UTC time.
fn rfc3339_ms(ms: i64) -> Option<String> {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

/// `roots` plus every added account's config directory, where an agent
/// started under that account writes its session log.
pub fn roots_with_accounts(store: &Store, roots: &SessionRoots) -> SessionRoots {
    let mut roots = roots.clone();
    let accounts = store.account_rows().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "could not read accounts for session logs");
        Vec::new()
    });
    for a in accounts {
        let Some(format) = SessionFormat::for_harness(crate::engine::engine_harness(&a.harness))
        else {
            continue;
        };
        let list = roots.for_format_mut(format);
        let dir = PathBuf::from(a.config_dir);
        if !list.contains(&dir) {
            list.push(dir);
        }
    }
    roots
}

/// The most recently written session log for an agent working in `cwd`,
/// among `formats`, and written at or after `since` when given.
pub fn locate_any(
    cwd: &Path,
    formats: &[SessionFormat],
    roots: &SessionRoots,
    since: Option<SystemTime>,
) -> Option<(PathBuf, SessionFormat)> {
    formats
        .iter()
        .filter_map(|&f| locate_since(f, cwd, roots, since).map(|p| (p, f)))
        .max_by_key(|(p, _)| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH)
        })
}
