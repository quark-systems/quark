//! The transcript tap: projects harness session logs into
//! `coordinator.message` and `worker.transcript` events.
//!
//! A worker's log is found from its worktree and its harness; a coordinator's
//! from its Project's workspace root, trying every supported harness. Only
//! byte offsets are stored (`transcript_index`), so deleting the database
//! re-reads every log from the start.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use quark_transcript::{locate, read_from, SessionFormat, SessionRoots};

use crate::store::{Store, TranscriptSource};

/// How long a located log is trusted before looking again, so a relaunched
/// agent's new log is picked up without rescanning harness directories on
/// every tick.
const RELOCATE_AFTER: Duration = Duration::from_secs(10);

type Located = Option<(PathBuf, SessionFormat)>;

pub struct TranscriptTap {
    store: Arc<Store>,
    roots: SessionRoots,
    located: Mutex<HashMap<TranscriptSource, (Instant, Located)>>,
}

impl TranscriptTap {
    pub fn new(store: Arc<Store>, roots: SessionRoots) -> Self {
        Self {
            store,
            roots,
            located: Mutex::new(HashMap::new()),
        }
    }

    /// Reads whatever `cwd`'s session log gained since the last call and
    /// appends it as events. Blocking: call from a blocking task.
    pub fn poll(
        &self,
        project_id: &str,
        source: TranscriptSource,
        cwd: &Path,
        formats: &[SessionFormat],
    ) -> anyhow::Result<()> {
        let Some((path, format)) = self.locate(&source, cwd, formats) else {
            return Ok(());
        };
        let path_str = path.to_string_lossy().into_owned();
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
        if moved {
            let entries: Vec<_> = batch.entries.into_iter().map(|(_, e)| e).collect();
            self.store.apply_transcript(
                project_id,
                &source,
                &path_str,
                batch.next_offset,
                &entries,
            )?;
        }
        Ok(())
    }

    fn locate(&self, source: &TranscriptSource, cwd: &Path, formats: &[SessionFormat]) -> Located {
        if let Some((at, found)) = self.located.lock().unwrap().get(source) {
            if at.elapsed() < RELOCATE_AFTER {
                return found.clone();
            }
        }
        let found = formats
            .iter()
            .filter_map(|&f| locate(f, cwd, &self.roots).map(|p| (p, f)))
            .max_by_key(|(p, _)| {
                std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH)
            });
        self.located
            .lock()
            .unwrap()
            .insert(source.clone(), (Instant::now(), found.clone()));
        found
    }
}
