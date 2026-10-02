//! `state/<id>.status` lines: append-only wake events written by workers.
//!
//! Grammar (owned by the engine's `bin/fm-classify-lib.sh`):
//!
//! ```text
//! <verb>[ corr=<16 hex>][ [name=value]...]: <note>
//! <verb>: [key=<slug>] <note>          (note-head key, equivalent position)
//! ```
//!
//! A line is history, not current state; the snapshot's `current_state` is the
//! reconciled truth. Parsing here matches the engine's verb, key and note rules
//! so the daemon's decision projection cannot drift from the engine's fold.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The decision key a line states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "slug", rename_all = "snake_case")]
pub enum DecisionKey {
    /// No key token: the engine's shared "default" bucket.
    Default,
    Stated(String),
    /// A key token whose slug fails `A-Za-z0-9._-`. The engine's fold skips the line.
    Invalid(String),
}

impl DecisionKey {
    /// The key the engine folds under, or `None` when the line is skipped.
    pub fn fold_key(&self) -> Option<&str> {
        match self {
            DecisionKey::Default => Some("default"),
            DecisionKey::Stated(s) => Some(s),
            DecisionKey::Invalid(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusEvent {
    /// Leading verb, e.g. `working`, `needs-decision`, `resolved`, `done`.
    pub verb: String,
    pub key: DecisionKey,
    /// Correlation token from a marked secondmate request, bracketed or not.
    pub corr: Option<String>,
    pub note: String,
    pub raw: String,
}

impl StatusEvent {
    /// Verbs that open a keyed decision in the engine's fold.
    pub fn opens_decision(&self) -> bool {
        matches!(self.verb.as_str(), "needs-decision" | "blocked")
    }

    /// Verbs that close a keyed decision in the engine's fold.
    pub fn closes_decision(&self) -> bool {
        matches!(self.verb.as_str(), "resolved" | "captain-held")
    }
}

/// Parse one status line. Returns `None` for blank lines.
pub fn parse_line(line: &str) -> Option<StatusEvent> {
    let raw = line.trim_end_matches(['\n', '\r']);
    if raw.trim().is_empty() {
        return None;
    }
    let (prefix, rest) = match raw.split_once(':') {
        Some((p, r)) => (p, Some(r)),
        None => (raw, None),
    };

    let head = prefix.split('[').next().unwrap_or("").trim();
    let (verb, unbracketed_corr) = strip_corr_tokens(head);

    let mut key = DecisionKey::Default;
    let mut note = match rest {
        Some(r) => r.trim_start().to_string(),
        None => raw.to_string(),
    };
    if let Some(slug) = key_token(prefix) {
        key = classify_slug(slug);
    } else if let Some(r) = rest {
        let r = r.trim_start();
        if let Some(slug) = key_token_at_head(r) {
            key = classify_slug(slug);
            if matches!(key, DecisionKey::Stated(_)) {
                note = r[format!("[key={slug}]").len()..].trim_start().to_string();
            }
        }
    }

    let corr = unbracketed_corr.or_else(|| bracket_tag(prefix, "corr").map(str::to_string));
    Some(StatusEvent {
        verb,
        key,
        corr,
        note,
        raw: raw.to_string(),
    })
}

fn is_corr_token(word: &str) -> bool {
    word.strip_prefix("corr=")
        .is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Keep the first word, drop exact `corr=<16 hex>` words after it.
fn strip_corr_tokens(head: &str) -> (String, Option<String>) {
    if !head.contains("corr=") {
        return (head.to_string(), None);
    }
    let mut words = head.split_whitespace();
    let mut out = words.next().unwrap_or("").to_string();
    let mut corr = None;
    for w in words {
        if is_corr_token(w) {
            corr.get_or_insert_with(|| w["corr=".len()..].to_string());
        } else {
            out.push(' ');
            out.push_str(w);
        }
    }
    (out, corr)
}

/// Value of a complete `[name=value]` tag anywhere in `s`.
fn bracket_tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("[{name}=");
    let start = s.find(&open)? + open.len();
    let len = s[start..].find(']')?;
    Some(&s[start..start + len])
}

fn key_token(prefix: &str) -> Option<&str> {
    bracket_tag(prefix, "key")
}

fn key_token_at_head(note: &str) -> Option<&str> {
    let body = note.strip_prefix("[key=")?;
    let len = body.find(']')?;
    Some(&body[..len])
}

fn classify_slug(slug: &str) -> DecisionKey {
    let ok = !slug.is_empty()
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        DecisionKey::Stated(slug.to_string())
    } else {
        DecisionKey::Invalid(slug.to_string())
    }
}

/// One complete line read from a status log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLine {
    /// Byte offset of the line in the file; stable across reads, usable as a dedupe key.
    pub offset: u64,
    pub event: StatusEvent,
}

/// Incremental reader over one status log.
///
/// Each [`StatusTail::read_new`] returns complete lines appended since the last
/// call. A partial trailing line waits for its newline. If the file is replaced
/// or truncated, reading restarts at byte 0 (a bounded duplicate beats a lost
/// event, the same trade the engine makes). A missing file reads as empty.
#[derive(Debug, Clone)]
pub struct StatusTail {
    path: PathBuf,
    offset: u64,
    identity: Option<(u64, u64)>,
}

impl StatusTail {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            identity: None,
        }
    }

    /// Resume from a previously persisted cursor.
    pub fn resume(path: impl Into<PathBuf>, offset: u64) -> Self {
        Self {
            offset,
            ..Self::new(path)
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Byte offset just past the last complete line returned.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn read_new(&mut self) -> Result<Vec<StatusLine>> {
        let io = |source| Error::Io {
            path: self.path.clone(),
            source,
        };
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io(e)),
        };
        let meta = file.metadata().map_err(io)?;
        let identity = (meta.dev(), meta.ino());
        if self.identity.is_some_and(|id| id != identity) || meta.len() < self.offset {
            self.offset = 0;
        }
        self.identity = Some(identity);

        file.seek(SeekFrom::Start(self.offset)).map_err(io)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).map_err(io)?;
        let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') else {
            return Ok(Vec::new());
        };

        let mut lines = Vec::new();
        let mut pos = 0usize;
        for chunk in buf[..=last_nl].split_inclusive(|&b| b == b'\n') {
            let text = String::from_utf8_lossy(chunk);
            if let Some(event) = parse_line(&text) {
                lines.push(StatusLine {
                    offset: self.offset + pos as u64,
                    event,
                });
            }
            pos += chunk.len();
        }
        self.offset += pos as u64;
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn p(line: &str) -> StatusEvent {
        parse_line(line).unwrap()
    }

    #[test]
    fn plain_line() {
        let e = p("working: building the thing\n");
        assert_eq!(e.verb, "working");
        assert_eq!(e.key, DecisionKey::Default);
        assert_eq!(e.note, "building the thing");
        assert_eq!(e.corr, None);
    }

    #[test]
    fn key_before_colon() {
        let e = p("needs-decision [key=api-shape]: pick one");
        assert_eq!(e.verb, "needs-decision");
        assert_eq!(e.key, DecisionKey::Stated("api-shape".into()));
        assert_eq!(e.note, "pick one");
        assert!(e.opens_decision());
    }

    #[test]
    fn key_at_note_head_is_stripped() {
        let e = p("needs-decision: [key=api-shape] pick one");
        assert_eq!(e.key, DecisionKey::Stated("api-shape".into()));
        assert_eq!(e.note, "pick one");
    }

    #[test]
    fn before_colon_key_wins_and_note_keeps_head_token() {
        let e = p("resolved [key=a]: [key=b] went with a");
        assert_eq!(e.key, DecisionKey::Stated("a".into()));
        assert_eq!(e.note, "[key=b] went with a");
        assert!(e.closes_decision());
    }

    #[test]
    fn key_deeper_in_note_is_prose() {
        let e = p("needs-decision: should we reuse [key=x]?");
        assert_eq!(e.key, DecisionKey::Default);
    }

    #[test]
    fn invalid_slug_is_skipped_by_fold() {
        let e = p("needs-decision [key=bad slug]: x");
        assert_eq!(e.key, DecisionKey::Invalid("bad slug".into()));
        assert_eq!(e.key.fold_key(), None);
    }

    #[test]
    fn unbracketed_corr_token() {
        let e = p("needs-decision corr=0123456789abcdef [key=texte-du-mur]: s");
        assert_eq!(e.verb, "needs-decision");
        assert_eq!(e.corr.as_deref(), Some("0123456789abcdef"));
        assert_eq!(e.key, DecisionKey::Stated("texte-du-mur".into()));
    }

    #[test]
    fn bracketed_corr_tag() {
        let e = p("done [corr=0123456789ABCDEF]: shipped");
        assert_eq!(e.verb, "done");
        assert_eq!(e.corr.as_deref(), Some("0123456789ABCDEF"));
    }

    #[test]
    fn malformed_corr_keeps_extra_words() {
        let e = p("resolved corr=123 [key=k]: x");
        assert_eq!(e.verb, "resolved corr=123");
        assert!(!e.closes_decision());
        let e = p("corr=0123456789abcdef resolved: x");
        assert_eq!(e.verb, "corr=0123456789abcdef resolved");
    }

    #[test]
    fn no_colon_and_blank() {
        let e = p("just text");
        assert_eq!(e.verb, "just text");
        assert_eq!(e.note, "just text");
        assert!(parse_line("   \n").is_none());
    }

    #[test]
    fn tail_reads_increments_and_waits_for_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.status");
        let mut tail = StatusTail::new(&path);
        assert!(tail.read_new().unwrap().is_empty());

        let mut f = File::create(&path).unwrap();
        write!(f, "working: a\nneeds-decision [key=k]: b\npaus").unwrap();
        let got = tail.read_new().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].offset, 0);
        assert_eq!(got[1].offset, 11);
        assert_eq!(got[1].event.key, DecisionKey::Stated("k".into()));
        assert!(tail.read_new().unwrap().is_empty());

        writeln!(f, "ed: c").unwrap();
        let got = tail.read_new().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].event.verb, "paused");
        assert_eq!(tail.offset(), std::fs::metadata(&path).unwrap().len());

        let resumed = StatusTail::resume(&path, tail.offset()).read_new().unwrap();
        assert!(resumed.is_empty());
    }

    #[test]
    fn tail_restarts_on_truncate_or_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.status");
        std::fs::write(&path, "working: a long first line\n").unwrap();
        let mut tail = StatusTail::new(&path);
        assert_eq!(tail.read_new().unwrap().len(), 1);

        std::fs::write(&path, "done: x\n").unwrap();
        let got = tail.read_new().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].event.verb, "done");

        let tmp = dir.path().join("new");
        std::fs::write(&tmp, "done: x\nfailed: y\n").unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        let got = tail.read_new().unwrap();
        assert_eq!(got.len(), 2, "replacement re-reads from byte 0");
    }
}
