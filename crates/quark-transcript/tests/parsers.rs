use std::path::{Path, PathBuf};

use quark_transcript::{
    locate, read_from, SessionFormat, SessionRoots, TranscriptEntry, TranscriptRole,
};

use TranscriptRole::{Assistant, Thinking, ToolCall, ToolResult, User};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn parse(format: SessionFormat, name: &str) -> Vec<TranscriptEntry> {
    let batch = read_from(&fixture(name), 0, format).unwrap();
    assert_eq!(batch.malformed, 0);
    batch.entries.into_iter().map(|(_, e)| e).collect()
}

fn shape(entries: &[TranscriptEntry]) -> Vec<(TranscriptRole, &str, Option<&str>, bool)> {
    entries
        .iter()
        .map(|e| (e.role, e.text.as_str(), e.tool_name.as_deref(), e.is_error))
        .collect()
}

#[test]
fn claude_session_log() {
    let entries = parse(SessionFormat::Claude, "claude.jsonl");
    assert_eq!(
        shape(&entries),
        vec![
            (User, "Fix #42 and add tests for the parser", None, false),
            (Thinking, "Read the parser first.", None, false),
            (
                ToolCall,
                r#"{"file_path":"src/parser.rs"}"#,
                Some("Read"),
                false
            ),
            (ToolResult, "fn parse() {}", None, false),
            (ToolCall, r#"{"command":"cargo test"}"#, Some("Bash"), false),
            (ToolResult, "1 failed", None, true),
            (
                Assistant,
                "The test fails on empty input; fixing it now.",
                None,
                false
            ),
        ]
    );
    assert_eq!(entries[2].tool_call_id.as_deref(), Some("toolu_1"));
    assert_eq!(entries[3].tool_call_id.as_deref(), Some("toolu_1"));
    assert_eq!(entries[0].ts.as_deref(), Some("2026-10-02T10:00:01.000Z"));
}

#[test]
fn codex_rollout() {
    let entries = parse(SessionFormat::Codex, "codex.jsonl");
    assert_eq!(
        shape(&entries),
        vec![
            (User, "Fix #42 and add tests for the parser", None, false),
            (Thinking, "**Reading the parser**", None, false),
            (
                ToolCall,
                r#"{"command":["cargo","test"]}"#,
                Some("shell"),
                false
            ),
            (ToolResult, "1 failed", None, false),
            (
                ToolCall,
                "*** Begin Patch\n*** End Patch",
                Some("apply_patch"),
                false
            ),
            (ToolResult, "Success.", None, false),
            (Assistant, "Fixed the empty-input case.", None, false),
        ]
    );
    assert_eq!(entries[2].tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(entries[3].tool_call_id.as_deref(), Some("call_1"));
}

#[test]
fn pi_session_log() {
    let entries = parse(SessionFormat::Pi, "pi.jsonl");
    assert_eq!(
        shape(&entries),
        vec![
            (User, "Fix #42 and add tests for the parser", None, false),
            (Thinking, "Read the parser first.", None, false),
            (ToolCall, r#"{"path":"src/parser.rs"}"#, Some("read"), false),
            (ToolResult, "fn parse() {}", Some("read"), false),
            (ToolCall, "cargo test", Some("bash"), false),
            (ToolResult, "1 failed", Some("bash"), true),
            (Assistant, "Fixed the empty-input case.", None, false),
        ]
    );
    assert_eq!(entries[3].tool_call_id.as_deref(), Some("tc_1"));
}

#[test]
fn locates_each_harness_log_by_working_directory() {
    let home = tempfile::tempdir().unwrap();
    let cwd = Path::new("/work/fix-42");
    let roots = SessionRoots {
        claude: vec![home.path().join(".claude")],
        codex: vec![home.path().join(".codex")],
        pi: vec![home.path().join(".pi/agent")],
    };
    for f in SessionFormat::ALL {
        assert_eq!(locate(f, cwd, &roots), None);
    }

    let claude_dir = home.path().join(".claude/projects/-work-fix-42");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let old = claude_dir.join("old.jsonl");
    std::fs::write(&old, "").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let new = claude_dir.join("new.jsonl");
    std::fs::copy(fixture("claude.jsonl"), &new).unwrap();
    assert_eq!(locate(SessionFormat::Claude, cwd, &roots), Some(new));

    let pi_dir = home.path().join(".pi/agent/sessions/--work-fix-42--");
    std::fs::create_dir_all(&pi_dir).unwrap();
    let pi = pi_dir.join("2026-10-02T10-00-00-000Z_7a1e.jsonl");
    std::fs::copy(fixture("pi.jsonl"), &pi).unwrap();
    assert_eq!(locate(SessionFormat::Pi, cwd, &roots), Some(pi));

    // Codex keys rollouts by date, not directory: only the session_meta line
    // tells which working directory a rollout belongs to.
    let day = home.path().join(".codex/sessions/2026/10/02");
    std::fs::create_dir_all(&day).unwrap();
    let mine = day.join("rollout-2026-10-02T10-00-00-0199.jsonl");
    std::fs::copy(fixture("codex.jsonl"), &mine).unwrap();
    let other = day.join("rollout-2026-10-02T11-00-00-0200.jsonl");
    std::fs::write(
        &other,
        r#"{"timestamp":"t","type":"session_meta","payload":{"id":"0200","cwd":"/elsewhere"}}
"#,
    )
    .unwrap();
    assert_eq!(locate(SessionFormat::Codex, cwd, &roots), Some(mine));
}
