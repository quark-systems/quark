//! Incremental reads of an append-only session log.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use crate::{RateLimit, SessionFormat, TranscriptEntry, TranscriptRole};

/// Most bytes one [`read_from`] call consumes; the next call continues from
/// `next_offset`, so a large backlog is projected over several ticks.
pub const MAX_BATCH_BYTES: u64 = 4 * 1024 * 1024;

/// What one read found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReadBatch {
    /// Entries in log order, each with the byte offset of the line it came from.
    pub entries: Vec<(u64, TranscriptEntry)>,
    /// Offset to pass on the next call. Always at a line boundary: a line the
    /// harness is still writing is left for the next read.
    pub next_offset: u64,
    /// `true` when the file was shorter than `offset`, so it was replaced or
    /// truncated and was read again from the start.
    pub restarted: bool,
    /// Complete lines that were not JSON.
    pub malformed: usize,
    /// The rate limit the agent is stopped at: the last one the read found,
    /// when the read reached the end of the log and the agent wrote nothing
    /// of its own after it. A limit the agent already worked past is not
    /// reported.
    pub rate_limit: Option<RateLimit>,
}

/// Parses complete lines appended to `path` since byte `offset`.
pub fn read_from(path: &Path, offset: u64, format: SessionFormat) -> io::Result<ReadBatch> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut batch = ReadBatch::default();
    let mut pos = offset;
    if len < offset {
        batch.restarted = true;
        pos = 0;
    }
    file.seek(SeekFrom::Start(pos))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let start = pos;
    let mut at_end = false;
    while pos - start < MAX_BATCH_BYTES {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 || line.last() != Some(&b'\n') {
            at_end = true;
            break;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        if !text.is_empty() {
            match serde_json::from_str::<serde_json::Value>(text) {
                Ok(v) => {
                    let entries = format.parse_value(&v);
                    if let Some(limit) = format.rate_limit(&v) {
                        batch.rate_limit = Some(limit);
                    } else if entries.iter().any(|e| {
                        matches!(
                            e.role,
                            TranscriptRole::Assistant
                                | TranscriptRole::Thinking
                                | TranscriptRole::ToolCall
                        )
                    }) {
                        batch.rate_limit = None;
                    }
                    batch.entries.extend(entries.into_iter().map(|e| (pos, e)));
                }
                Err(_) => batch.malformed += 1,
            }
        }
        pos += n as u64;
    }
    if !at_end {
        batch.rate_limit = None;
    }
    batch.next_offset = pos;
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const USER: &str = r#"{"type":"user","message":{"role":"user","content":"hello"}}"#;
    const REPLY: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#;

    #[test]
    fn reads_only_complete_lines_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut f = File::create(&path).unwrap();
        write!(f, "{USER}\n{}", &REPLY[..20]).unwrap();
        f.flush().unwrap();

        let first = read_from(&path, 0, SessionFormat::Claude).unwrap();
        assert_eq!(first.entries.len(), 1);
        assert_eq!(first.entries[0].1.role, TranscriptRole::User);
        assert_eq!(first.next_offset, USER.len() as u64 + 1);

        writeln!(f, "{}", &REPLY[20..]).unwrap();
        f.flush().unwrap();
        let second = read_from(&path, first.next_offset, SessionFormat::Claude).unwrap();
        assert_eq!(second.entries.len(), 1);
        assert_eq!(second.entries[0].0, first.next_offset);
        assert_eq!(second.entries[0].1.text, "hi");
        assert!(!second.restarted);

        let none = read_from(&path, second.next_offset, SessionFormat::Claude).unwrap();
        assert!(none.entries.is_empty());
        assert_eq!(none.next_offset, second.next_offset);
    }

    #[test]
    fn restarts_when_the_file_shrank() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, format!("{USER}\n")).unwrap();
        let batch = read_from(&path, 10_000, SessionFormat::Claude).unwrap();
        assert!(batch.restarted);
        assert_eq!(batch.entries.len(), 1);
    }

    #[test]
    fn counts_malformed_lines_and_moves_past_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, format!("{{broken\n{USER}\n")).unwrap();
        let batch = read_from(&path, 0, SessionFormat::Claude).unwrap();
        assert_eq!(batch.malformed, 1);
        assert_eq!(batch.entries.len(), 1);
    }

    const LIMITED: &str = r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","message":{"role":"assistant","content":[{"type":"text","text":"You've hit your session limit"}]}}"#;

    #[test]
    fn reports_a_rate_limit_the_agent_is_stopped_at() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, format!("{USER}\n{REPLY}\n{LIMITED}\n{USER}\n")).unwrap();
        let batch = read_from(&path, 0, SessionFormat::Claude).unwrap();
        let limit = batch.rate_limit.expect("a rate limit");
        assert_eq!(
            limit.message.as_deref(),
            Some("You've hit your session limit")
        );
        // The harness's own message still reaches the transcript.
        assert_eq!(batch.entries.len(), 4);

        let none = read_from(&path, batch.next_offset, SessionFormat::Claude).unwrap();
        assert_eq!(none.rate_limit, None);
    }

    #[test]
    fn a_rate_limit_the_agent_worked_past_is_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, format!("{LIMITED}\n{USER}\n{REPLY}\n")).unwrap();
        let batch = read_from(&path, 0, SessionFormat::Claude).unwrap();
        assert_eq!(batch.rate_limit, None);
    }
}
