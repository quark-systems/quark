//! Wiring a Project coordinator that runs on the Antigravity CLI (`agy`).
//!
//! agy reads a workspace's own customizations from `<cwd>/.agents/`, so the
//! person's `~/.gemini` is left alone:
//!
//! - `.agents/mcp_config.json`: [`SERVER_NAME`] by URL (streamable HTTP),
//!   each tool eager, so agy declares it to the model as a tool of its own
//!   instead of listing its name and making the model read its schema from
//!   disk first. The coordinator runs with `--dangerously-skip-permissions`,
//!   as workers do, so no call asks.
//! - `.agents/hooks.json`: one named hook, [`HOOK_NAME`]. Its
//!   `PreInvocation` handler POSTs to `hooks/agy_prompt` and prints what
//!   quarkd answers: the coordinator's prompt to add ([`injection`]) or
//!   nothing. Its `Stop` handler POSTs to `hooks/agy_stop`, whose answer can
//!   send the turn back (`{"decision": "continue"}`). Each prints `{}` when
//!   quarkd cannot be reached, since agy reads a hook's output as JSON.
//!
//! agy has no system-prompt flag, and the places it takes instructions from
//! cut them short: a rule file at 24 KB, all rules at 20k tokens, a hook's
//! system message at about 48 KB. A Project's layered prompt runs from 35 KB
//! to over 100 KB, so quarkd adds it as system messages of at most
//! [`CHUNK`] bytes each, before the model's first call, again after each
//! compaction (which drops them), and again when the prompt file changed.
//! The first part carries [`MARK`] and the prompt's digest, which is how a
//! later reading of the transcript ([`PromptState`]) knows the prompt is
//! still in the conversation.
//!
//! agy also lists a workspace's MCP tools only from a conversation's second
//! run on, so the first `Stop` of each start of agy (told apart by
//! [`LAUNCH_ENV`]) sends the turn back once with [`TOOLS_NOTE`].

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::coordinator_http::{CoordinatorUrls, SERVER_NAME};

/// Quark's named hook in the workspace's `.agents/hooks.json`.
pub const HOOK_NAME: &str = "quark-coordinator";
/// The hook event that asks whether to add the prompt.
pub const PROMPT_EVENT: &str = "agy_prompt";
/// The hook event that ends a turn.
pub const STOP_EVENT: &str = "agy_stop";
/// Opens the first part of the prompt, before its digest.
pub const MARK: &str = "quark coordinator prompt";
/// The largest system message quarkd adds, in bytes; agy cuts one at
/// about 48 KB.
pub const CHUNK: usize = 30_000;
/// Names one start of the coordinator's agy process: set in its window's
/// environment and sent back by its hooks as [`LAUNCH_HEADER`].
pub const LAUNCH_ENV: &str = "QUARK_AGY_LAUNCH";
/// The header a hook sends [`LAUNCH_ENV`] in.
pub const LAUNCH_HEADER: &str = "x-quark-launch";
/// Why the first turn after a start is sent back once. agy 1.5 fixes a
/// conversation's tools when it starts, before it has connected the
/// workspace's MCP servers, and lists them from the next run of the turn
/// on, so the first turn after every start runs without Quark's tools.
pub const TOOLS_NOTE: &str = "Quark check: Quark's tools (mcp_quark-coordinator_*) were not connected when this turn started, and are now. If this turn needed them, use them now and finish it. Otherwise stop without saying anything more.";

/// The digest that names one version of the prompt text.
pub fn digest(text: &str) -> String {
    let d = Sha256::digest(text.as_bytes());
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// `text` as system messages of at most [`CHUNK`] bytes, cut at a line
/// when one is near, each saying which part it is. The first carries
/// [`MARK`] and `digest`.
pub fn chunks(text: &str, digest: &str) -> Vec<String> {
    let mut parts: Vec<&str> = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        if rest.len() <= CHUNK {
            parts.push(rest);
            break;
        }
        let mut end = CHUNK;
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        // A line break in the last quarter, so a part ends on a whole line.
        if let Some(nl) = rest[..end].rfind('\n').filter(|&nl| nl > CHUNK * 3 / 4) {
            end = nl + 1;
        }
        parts.push(&rest[..end]);
        rest = &rest[end..];
    }
    let n = parts.len();
    parts
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            if i == 0 {
                format!(
                    "<!-- {MARK} {digest} -->\nYour instructions as this Quark Project's coordinator follow, in {n} part(s). They come before anything else you have been told, and replace any earlier copy of them in this conversation.\n\n[Part 1 of {n}]\n{p}"
                )
            } else {
                format!("[Part {} of {n}]\n{p}", i + 1)
            }
        })
        .collect()
}

/// The `PreInvocation` answer that adds `text` to the conversation.
pub fn injection(text: &str, digest: &str) -> Value {
    let steps: Vec<Value> = chunks(text, digest)
        .into_iter()
        .map(|c| json!({ "systemMessage": { "systemMessage": c } }))
        .collect();
    json!({ "injectSteps": steps })
}

/// What one agy transcript says about the prompt, read incrementally.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptState {
    /// Bytes of the transcript read so far.
    pub offset: u64,
    /// The digest of the prompt still in the conversation: the last one
    /// added, unless a compaction came after it.
    pub holds: Option<String>,
}

impl PromptState {
    /// Read what `path` gained since the last call. A transcript shorter
    /// than what was read was rewritten, and is read again from its start.
    pub fn scan(&mut self, path: &Path) -> std::io::Result<()> {
        let mut f = std::fs::File::open(path)?;
        let len = f.metadata()?.len();
        if len < self.offset {
            *self = PromptState::default();
        }
        f.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(f.take(len - self.offset));
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line)?;
            if n == 0 || !line.ends_with('\n') {
                break; // the end, or a line still being written
            }
            self.offset += n as u64;
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let content = v.get("content").and_then(Value::as_str).unwrap_or("");
            match v.get("type").and_then(Value::as_str) {
                Some("SYSTEM_MESSAGE") => {
                    let mark = format!("<!-- {MARK} ");
                    if let Some(at) = content.find(&mark) {
                        let d = content[at + mark.len()..]
                            .split_whitespace()
                            .next()
                            .unwrap_or("");
                        self.holds = Some(d.to_string());
                    }
                }
                Some("CHECKPOINT") if quark_transcript::antigravity::is_compaction(content) => {
                    self.holds = None;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// The command a hook runs for `event`: POST agy's payload to quarkd and
/// print the answer, or `{}` when there is none.
fn hook_command(urls: &CoordinatorUrls, event: &str) -> String {
    format!(
        "out=$(curl -fsS -m 10 -X POST -H 'Content-Type: application/json' -H \"X-Quark-Launch: ${{{LAUNCH_ENV}:-}}\" --data-binary @- '{}/{event}' 2>/dev/null); case \"$out\" in '{{'*) printf '%s\\n' \"$out\" ;; *) echo '{{}}' ;; esac",
        urls.hooks
    )
}

/// Configure the agy coordinator running in `root` to reach quarkd, as the
/// module describes. Other servers and hooks in those files are kept, and
/// Quark's own entries replaced. Returns whether a file changed.
pub fn install(root: &Path, urls: &CoordinatorUrls) -> std::io::Result<bool> {
    let tools: serde_json::Map<String, Value> = quark_coordinator::tools::catalog()
        .iter()
        .map(|t| (t.name.to_string(), json!({ "eager": true })))
        .collect();
    let mut changed =
        crate::coordinator_http::edit_json(&root.join(".agents/mcp_config.json"), |v| {
            v["mcpServers"][SERVER_NAME] = json!({ "url": urls.mcp, "tools": tools });
        })?;
    changed |= crate::coordinator_http::edit_json(&root.join(".agents/hooks.json"), |v| {
        v[HOOK_NAME] = json!({
            "PreInvocation": [{
                "type": "command",
                "command": hook_command(urls, PROMPT_EVENT),
                "timeout": 15,
            }],
            "Stop": [{
                "type": "command",
                "command": hook_command(urls, STOP_EVENT),
                "timeout": 15,
            }],
        });
    })?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_prompt_goes_in_parts_under_the_limit() {
        let line = "The coordinator never acknowledges status; it acts through tools.\n";
        let text = format!("{}END-MARKER", line.repeat(1600));
        let d = digest(&text);
        let parts = chunks(&text, &d);
        assert!(parts.len() >= 4, "{}", parts.len());
        for p in &parts {
            assert!(p.len() <= CHUNK + 400, "{}", p.len());
        }
        assert!(parts[0].starts_with(&format!("<!-- {MARK} {d} -->")));
        assert!(parts.last().unwrap().ends_with("END-MARKER"));
        // Every line arrives whole, once.
        let joined: String = parts
            .iter()
            .map(|p| p.split_once("]\n").unwrap().1)
            .collect();
        assert_eq!(joined, text);
        // A short prompt is one part.
        assert_eq!(chunks("hello", "d").len(), 1);
        let inj = injection("hello", "d");
        assert!(inj["injectSteps"][0]["systemMessage"]["systemMessage"]
            .as_str()
            .unwrap()
            .contains("hello"));
    }

    #[test]
    fn the_transcript_says_whether_the_prompt_is_still_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript_full.jsonl");
        let line = |v: Value| format!("{v}\n");
        let user = json!({"step_index": 0, "source": "USER_EXPLICIT", "type": "USER_INPUT",
            "content": "<USER_REQUEST>\nhi\n</USER_REQUEST>"});
        let added = |d: &str| {
            json!({"source": "SYSTEM_SDK", "type": "SYSTEM_MESSAGE",
            "content": format!("The following is a <SYSTEM_MESSAGE>...\n<!-- {MARK} {d} -->\nYour instructions")})
        };
        let compacted = json!({"source": "SYSTEM", "type": "CHECKPOINT",
            "content": "# Resuming from a compaction\n\nYou are continuing"});
        std::fs::write(&path, line(user.clone())).unwrap();
        let mut s = PromptState::default();
        s.scan(&path).unwrap();
        assert_eq!(s.holds, None);

        let mut text = line(user.clone()) + &line(added("aaaa"));
        std::fs::write(&path, &text).unwrap();
        s.scan(&path).unwrap();
        assert_eq!(s.holds.as_deref(), Some("aaaa"));

        // A half-written line waits.
        text += &line(compacted.clone());
        std::fs::write(&path, format!("{text}{{\"type\": \"SYS")).unwrap();
        s.scan(&path).unwrap();
        assert_eq!(s.holds, None, "the compaction dropped it");
        text += &line(added("bbbb"));
        std::fs::write(&path, &text).unwrap();
        s.scan(&path).unwrap();
        assert_eq!(s.holds.as_deref(), Some("bbbb"));

        // A rewritten, shorter transcript is read again from its start.
        std::fs::write(&path, line(user) + &line(added("cccc"))).unwrap();
        s.scan(&path).unwrap();
        assert_eq!(s.holds.as_deref(), Some("cccc"));
    }

    #[test]
    fn installs_the_workspace_customizations_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let urls = CoordinatorUrls::new("http://127.0.0.1:7777", "p1", "tok");
        std::fs::create_dir_all(root.join(".agents")).unwrap();
        std::fs::write(
            root.join(".agents/hooks.json"),
            r#"{"theirs":{"Stop":[{"command":"x"}]}}"#,
        )
        .unwrap();
        assert!(install(root, &urls).unwrap());
        assert!(!install(root, &urls).unwrap(), "idempotent");
        let read = |p: &str| -> Value {
            serde_json::from_str(&std::fs::read_to_string(root.join(p)).unwrap()).unwrap()
        };
        let mcp = read(".agents/mcp_config.json");
        let server = &mcp["mcpServers"][SERVER_NAME];
        assert_eq!(server["url"], urls.mcp);
        assert_eq!(server["tools"]["ask_user"]["eager"], true);
        let hooks = read(".agents/hooks.json");
        assert_eq!(hooks["theirs"]["Stop"][0]["command"], "x");
        let pre = hooks[HOOK_NAME]["PreInvocation"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            pre.contains(&format!("{}/{PROMPT_EVENT}", urls.hooks)),
            "{pre}"
        );
        let stop = hooks[HOOK_NAME]["Stop"][0]["command"].as_str().unwrap();
        assert!(
            stop.contains(&format!("{}/{STOP_EVENT}", urls.hooks)),
            "{stop}"
        );
    }

    #[test]
    fn a_hook_prints_quarkds_answer_or_an_empty_object() {
        let dir = tempfile::tempdir().unwrap();
        let curl = dir.path().join("curl");
        let path = format!("{}:/usr/bin:/bin", dir.path().display());
        let urls = CoordinatorUrls::new("http://127.0.0.1:9", "p1", "tok");
        let run = |script: &str| {
            std::fs::write(&curl, script).unwrap();
            std::fs::set_permissions(&curl, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(hook_command(&urls, STOP_EVENT))
                .env("PATH", &path)
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(out.status.success());
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        assert_eq!(
            run("#!/bin/sh\necho '{\"decision\":\"continue\"}'\n"),
            r#"{"decision":"continue"}"#
        );
        assert_eq!(run("#!/bin/sh\nexit 7\n"), "{}");
        assert_eq!(run("#!/bin/sh\necho 'not json'\n"), "{}");
    }
}
