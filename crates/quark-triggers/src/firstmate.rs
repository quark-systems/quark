//! Today's bash engine, as far as slice 7 is concerned.
//!
//! - [`read_home`] reads a firstmate home's inbox notes
//!   (`state/inbox/*.note`, handled ones under `handled/`) and its away
//!   posture (`state/.afk`, `state/.afk-contract`), so the engine can
//!   mirror them into the log while firstmate still owns them.
//! - [`bash_escalates`] is firstmate's away classifier for a status line
//!   (`status_is_captain_relevant` in `bin/fm-classify-lib.sh`), the
//!   decision shadow mode compares the native route against.

use std::path::Path;

use quark_core::{CoreError, Result};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::away::{Occasion, Posture};
use crate::channel::{Inbound, INBOX};

/// The event kind the slice 1 bridge (`quark_eventlog::firstmate`) gives
/// each status line. Its payload carries at least `verb` and `raw`.
pub const STATUS_KIND: &str = "firstmate.status";

/// The fields of a `firstmate.status` payload routing needs.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct StatusLine {
    pub verb: String,
    #[serde(default)]
    pub note: String,
    pub raw: String,
}

/// Free-text phrases firstmate treats as captain-relevant on a line whose
/// verb it does not know (`FM_CLASSIFY_CAPTAIN_RE_DEFAULT`, matched
/// anywhere, ignoring case).
const LEGACY_PHRASES: [&str; 8] = [
    "done:",
    "needs-decision:",
    "blocked:",
    "failed:",
    "pr ready",
    "checks green",
    "ready in branch",
    "merged",
];

/// Whether firstmate's away daemon escalates this status line to the
/// coordinator. A port of `status_is_captain_relevant` with the default
/// pattern.
pub fn bash_escalates(verb: &str, raw: &str) -> bool {
    match verb {
        "working" | "resolved" | "captain-held" | "paused" => false,
        "done" | "needs-decision" | "blocked" | "failed" => true,
        _ => {
            let lower = raw.to_lowercase();
            LEGACY_PHRASES.iter().any(|p| lower.contains(p))
        }
    }
}

/// The occasion a firstmate status line stands for.
pub fn status_occasion(verb: &str, raw: &str) -> Occasion {
    match verb {
        "done" => Occasion::Done,
        "needs-decision" => Occasion::Decision,
        "blocked" => Occasion::Blocked,
        "failed" => Occasion::Failed,
        "paused" => Occasion::Paused,
        "working" | "resolved" | "captain-held" => Occasion::Progress,
        // firstmate's legacy free-text lines (`PR ready ...`) announce a
        // finished deliverable; anything else is progress.
        _ if bash_escalates(verb, raw) => Occasion::Done,
        _ => Occasion::Progress,
    }
}

/// What one firstmate home holds for slice 7.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Home {
    /// Notes still waiting in `state/inbox/`.
    pub pending: Vec<Inbound>,
    /// Notes moved to `state/inbox/handled/`.
    pub handled: Vec<Inbound>,
    pub posture: Posture,
    /// `expected_return` from the away record, when it has one.
    pub expected_return: Option<OffsetDateTime>,
}

fn io(path: &Path, e: impl std::fmt::Display) -> CoreError {
    CoreError::Backend(format!("{}: {e}", path.display()))
}

/// Read the inbox and away posture of the firstmate home at `home`.
pub fn read_home(home: &Path) -> Result<Home> {
    let state = home.join("state");
    let inbox = state.join("inbox");
    let mut out = Home {
        pending: read_notes(&inbox)?,
        handled: read_notes(&inbox.join("handled"))?,
        ..Home::default()
    };
    // A note in both places is mid-ack; it counts as handled.
    out.pending
        .retain(|n| !out.handled.iter().any(|h| h.id == n.id));
    let afk = state.join(".afk");
    match std::fs::read_to_string(&afk) {
        Ok(s) => {
            out.posture = match s.lines().next().map(str::trim) {
                Some("quiet") => Posture::Quiet,
                // An empty flag file is away, as fm_afk_mode reads it.
                _ => Posture::Away,
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // The away record alone is the posture on harnesses without
            // the daemon.
            if state.join(".afk-contract").exists() {
                out.posture = Posture::Away;
            }
        }
        Err(e) => return Err(io(&afk, e)),
    }
    if let Ok(s) = std::fs::read_to_string(state.join(".afk-contract")) {
        out.expected_return = s
            .lines()
            .find_map(|l| l.strip_prefix("expected_return:"))
            .and_then(|v| OffsetDateTime::parse(v.trim(), &Rfc3339).ok());
    }
    Ok(out)
}

fn read_notes(dir: &Path) -> Result<Vec<Inbound>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(dir, e)),
    };
    let mut notes = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| io(dir, e))?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("note") {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            // Acked between the listing and the read.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io(&path, e)),
        };
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        notes.push(parse_note(stem, &text));
    }
    notes.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
    Ok(notes)
}

/// Parse one `fm-inbox.sh` note: `key=value` header lines, `--`, then the
/// body. The file name is the id when the header lacks one.
pub fn parse_note(file_id: &str, text: &str) -> Inbound {
    let (head, body) = match text.split_once("\n--\n") {
        Some((h, b)) => (h, b),
        None => ("", text),
    };
    let field = |k: &str| {
        head.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
            .map(str::trim)
    };
    let at = field("at")
        .and_then(|v| OffsetDateTime::parse(v, &Rfc3339).ok())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    Inbound {
        id: field("id")
            .filter(|v| !v.is_empty())
            .unwrap_or(file_id)
            .to_string(),
        channel: INBOX.to_string(),
        from: field("source").unwrap_or("user").to_string(),
        body: body.strip_suffix('\n').unwrap_or(body).to_string(),
        at,
        task: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_matches_firstmate() {
        // (verb, line, escalates) from fm-classify-lib.sh's rules.
        let cases = [
            (
                "working",
                "working: merged the helper, PR ready soon",
                false,
            ),
            ("resolved", "resolved: key=a answered", false),
            ("paused", "paused: waiting on CI", false),
            ("captain-held", "captain-held: x", false),
            ("done", "done: PR https://x checks green", true),
            ("needs-decision", "needs-decision: key=k which?", true),
            ("blocked", "blocked: no creds", true),
            ("failed", "failed: tests", true),
            ("note", "note: PR ready for review", true),
            ("note", "note: Merged upstream", true),
            ("note", "note: still going", false),
        ];
        for (verb, line, want) in cases {
            assert_eq!(bash_escalates(verb, line), want, "{line}");
            assert_eq!(
                status_occasion(verb, line) != Occasion::Progress
                    && status_occasion(verb, line) != Occasion::Paused,
                want,
                "{line}"
            );
        }
    }

    #[test]
    fn reads_a_home() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("state/inbox");
        std::fs::create_dir_all(inbox.join("handled")).unwrap();
        std::fs::write(
            inbox.join("1700000000-aaaa.note"),
            "id=1700000000-aaaa\nat=2026-10-07T01:00:00Z\nsource=voice\n--\nship it\nplease\n",
        )
        .unwrap();
        std::fs::write(
            inbox.join("handled/1699999999-bbbb.note"),
            "id=1699999999-bbbb\nat=2026-10-06T01:00:00Z\nsource=cli\n--\nold\n",
        )
        .unwrap();
        std::fs::write(inbox.join(".staging-zz"), "half written").unwrap();
        let h = read_home(dir.path()).unwrap();
        assert_eq!(h.pending.len(), 1);
        assert_eq!(h.pending[0].id, "1700000000-aaaa");
        assert_eq!(h.pending[0].from, "voice");
        assert_eq!(h.pending[0].body, "ship it\nplease");
        assert_eq!(h.handled.len(), 1);
        assert_eq!(h.posture, Posture::Present);

        std::fs::write(dir.path().join("state/.afk"), "quiet\n").unwrap();
        assert_eq!(read_home(dir.path()).unwrap().posture, Posture::Quiet);
        std::fs::write(dir.path().join("state/.afk"), "").unwrap();
        std::fs::write(
            dir.path().join("state/.afk-contract"),
            "version: 1\nexpected_return: 2026-10-08T09:00:00Z\n",
        )
        .unwrap();
        let h = read_home(dir.path()).unwrap();
        assert_eq!(h.posture, Posture::Away);
        assert!(h.expected_return.is_some());
    }
}
