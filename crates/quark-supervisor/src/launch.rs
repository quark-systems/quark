//! Turning an assignment and a harness manifest into what a session runs.
//!
//! Pure functions, so the launch command, the worker's environment and the
//! instructions it reads can be tested without starting anything.

use std::collections::BTreeMap;
use std::path::Path;

use quark_core::harness::HarnessManifest;
use quark_core::{CoreError, ProjectId, Result, TaskId};
use quark_harness::LaunchVars;

use crate::fleet::Steer;

/// The worker's instructions, inside its generation directory.
pub const BRIEF_FILE: &str = "brief.md";
/// The worker's status file (the file-protocol fallback).
pub const STATUS_FILE: &str = "status";

/// Environment every worker gets, so it can find its way back to Quark
/// whatever transport its harness supports.
pub mod env {
    pub const PROJECT: &str = "QUARK_PROJECT";
    pub const TASK: &str = "QUARK_TASK";
    pub const GENERATION: &str = "QUARK_GENERATION";
    /// The status file to append `<state>: <note>` lines to.
    pub const STATUS_FILE: &str = "QUARK_STATUS_FILE";
    /// The worker's MCP endpoint, when quarkd serves the worker routes.
    pub const MCP_URL: &str = "QUARK_MCP_URL";
    /// The prefix hook commands POST to: `<url>/<event>`.
    pub const HOOK_URL: &str = "QUARK_HOOK_URL";
}

/// Where a worker reaches the worker protocol over HTTP: the base URL the
/// `quark_worker::router` is nested under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerUrls {
    pub base: String,
}

impl WorkerUrls {
    pub fn mcp(&self, task: &TaskId, generation: &str) -> String {
        format!(
            "{}/mcp/{task}/{generation}",
            self.base.trim_end_matches('/')
        )
    }

    pub fn hooks(&self, task: &TaskId, generation: &str) -> String {
        format!(
            "{}/hooks/{task}/{generation}",
            self.base.trim_end_matches('/')
        )
    }
}

/// Everything about one generation's launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// The instructions; written to [`BRIEF_FILE`] and, unless the harness
    /// takes them on its command line, typed in once it is ready.
    pub brief: String,
    /// Whether the brief must be typed in after start (`paste` and `stdin`
    /// harnesses).
    pub type_brief: bool,
}

/// What a generation is launched with.
pub struct Inputs<'a> {
    pub manifest: &'a HarnessManifest,
    pub project: &'a ProjectId,
    pub task: &'a TaskId,
    pub generation: &'a str,
    pub dir: &'a Path,
    pub worktree: &'a Path,
    pub brief: &'a str,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    /// Said to a relaunched worker before the brief.
    pub note: Option<&'a str>,
    /// Steering messages its predecessors never received.
    pub steers: &'a [Steer],
    pub extra_env: &'a BTreeMap<String, String>,
    pub urls: Option<&'a WorkerUrls>,
}

/// The launch plan for one generation.
pub fn plan(i: &Inputs<'_>) -> Result<Plan> {
    let m = i.manifest;
    if let Some(model) = i.model {
        if m.models.selection == "listed" && !m.models.known.iter().any(|k| k == model) {
            return Err(CoreError::Invalid(format!(
                "harness {} does not list model {model}",
                m.id
            )));
        }
    }
    if let Some(effort) = i.effort {
        if !m.efforts.iter().any(|e| e == effort) {
            return Err(CoreError::Invalid(format!(
                "harness {} has no effort {effort}",
                m.id
            )));
        }
    }
    let status = i.dir.join(STATUS_FILE);
    let brief = compose_brief(i, &status);
    let brief_path = i.dir.join(BRIEF_FILE);
    let model = match m.models.selection.as_str() {
        "automatic" | "none" => None,
        _ => i.model.map(str::to_string).or(m.models.default.clone()),
    };
    let on_argv = m.launch.prompt_via == "argv";
    let vars = LaunchVars {
        model,
        effort: i.effort.map(str::to_string),
        prompt: on_argv.then(|| brief.clone()),
        prompt_file: on_argv.then(|| path_str(&brief_path)),
        cwd: Some(path_str(i.worktree)),
    };
    let argv = quark_harness::argv(m, &vars);

    let mut env = m.launch.env.clone();
    env.extend(i.extra_env.clone());
    env.insert(env::PROJECT.into(), i.project.to_string());
    env.insert(env::TASK.into(), i.task.to_string());
    env.insert(env::GENERATION.into(), i.generation.into());
    env.insert(env::STATUS_FILE.into(), path_str(&status));
    if let Some(urls) = i.urls {
        env.insert(env::MCP_URL.into(), urls.mcp(i.task, i.generation));
        env.insert(env::HOOK_URL.into(), urls.hooks(i.task, i.generation));
    }
    Ok(Plan {
        argv,
        env,
        brief,
        type_brief: !on_argv,
    })
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The note, the brief, any steering the worker missed, and how to report.
fn compose_brief(i: &Inputs<'_>, status: &Path) -> String {
    let mut out = String::new();
    if let Some(note) = i.note.filter(|n| !n.trim().is_empty()) {
        out.push_str(note.trim());
        out.push_str("\n\n");
    }
    out.push_str(i.brief.trim_end());
    out.push('\n');
    if !i.steers.is_empty() {
        out.push_str("\nMessages sent while you were not running, oldest first:\n");
        for s in i.steers {
            out.push_str("\n- ");
            out.push_str(s.text.trim());
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "\nReport progress by appending one line per event to {}, such as \
         `working: <note>`, `blocked: <reason>`, `needs-decision [key=<slug>]: <question>`, \
         `learned: <fact>` or `done: <PR url or summary>`.\n",
        status.display()
    ));
    out
}

/// The bytes a key name sends, for a manifest's `keys.interrupt`.
/// Accepts the tmux names manifests use: `Escape`, `Enter`, `Tab`,
/// `C-<letter>`, or a single character.
pub fn key_bytes(name: &str) -> Result<Vec<u8>> {
    let bytes = match name {
        "Escape" | "Esc" => vec![0x1b],
        "Enter" => vec![b'\r'],
        "Tab" => vec![b'\t'],
        "BSpace" => vec![0x7f],
        _ => {
            if let Some(c) = name.strip_prefix("C-").filter(|c| c.len() == 1) {
                let c = c.as_bytes()[0].to_ascii_lowercase();
                if c.is_ascii_lowercase() {
                    vec![c - b'a' + 1]
                } else {
                    return Err(CoreError::Invalid(format!("key {name:?}")));
                }
            } else if name.chars().count() == 1 {
                name.as_bytes().to_vec()
            } else {
                return Err(CoreError::Invalid(format!("key {name:?}")));
            }
        }
    };
    Ok(bytes)
}

/// `text` as a bracketed paste followed by Enter, the way a person pastes a
/// message into an agent's composer and sends it.
pub fn paste(text: &str) -> Vec<u8> {
    // A paste end marker inside the text would end the paste early.
    let clean = text.replace("\x1b[201~", "");
    let mut out = b"\x1b[200~".to_vec();
    out.extend_from_slice(clean.trim_end().as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out.push(b'\r');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_harness::ManifestRegistry;

    fn inputs<'a>(
        ids: &'a (ProjectId, TaskId),
        m: &'a HarnessManifest,
        steers: &'a [Steer],
        env: &'a BTreeMap<String, String>,
        urls: Option<&'a WorkerUrls>,
    ) -> Inputs<'a> {
        Inputs {
            manifest: m,
            project: &ids.0,
            task: &ids.1,
            generation: "g1",
            dir: Path::new("/state/p/t1/g1"),
            worktree: Path::new("/pool/wt-1"),
            brief: "Fix the parser.",
            model: None,
            effort: Some("high"),
            note: None,
            steers,
            extra_env: env,
            urls,
        }
    }

    #[test]
    fn argv_harness_gets_brief_on_command_line() {
        let reg = ManifestRegistry::builtin();
        let ids = (ProjectId::from("p"), TaskId::from("t1"));
        let m = reg.get("codex").unwrap();
        let env = BTreeMap::new();
        let p = plan(&inputs(&ids, m, &[], &env, None)).unwrap();
        assert!(!p.type_brief);
        assert_eq!(p.argv[0], "codex");
        assert!(p.argv.last().unwrap().starts_with("Fix the parser."));
        assert_eq!(p.env[env::TASK], "t1");
        assert_eq!(p.env[env::STATUS_FILE], "/state/p/t1/g1/status");
        assert!(!p.env.contains_key(env::MCP_URL));
    }

    #[test]
    fn paste_harness_types_brief_and_gets_urls() {
        let reg = ManifestRegistry::builtin();
        let ids = (ProjectId::from("p"), TaskId::from("t1"));
        let m = reg.get("claude-code").unwrap();
        let env = BTreeMap::from([("CLAUDE_CONFIG_DIR".to_string(), "/a".to_string())]);
        let urls = WorkerUrls {
            base: "http://127.0.0.1:7777/v1/worker/".into(),
        };
        let steers = [Steer {
            id: "s1".into(),
            text: "use the new API".into(),
            delivered_to: None,
        }];
        let mut i = inputs(&ids, m, &steers, &env, Some(&urls));
        i.note = Some("You were restarted.");
        let p = plan(&i).unwrap();
        assert_eq!(p.type_brief, m.launch.prompt_via != "argv");
        assert!(p
            .brief
            .starts_with("You were restarted.\n\nFix the parser."));
        assert!(p.brief.contains("- use the new API"));
        assert_eq!(
            p.env[env::MCP_URL],
            "http://127.0.0.1:7777/v1/worker/mcp/t1/g1"
        );
        assert_eq!(p.env["CLAUDE_CONFIG_DIR"], "/a");
    }

    #[test]
    fn refuses_unknown_effort() {
        let reg = ManifestRegistry::builtin();
        let ids = (ProjectId::from("p"), TaskId::from("t1"));
        let m = reg.get("codex").unwrap();
        let env = BTreeMap::new();
        let mut i = inputs(&ids, m, &[], &env, None);
        i.effort = Some("ludicrous");
        assert!(matches!(plan(&i), Err(CoreError::Invalid(_))));
    }

    #[test]
    fn key_names() {
        assert_eq!(key_bytes("Escape").unwrap(), [0x1b]);
        assert_eq!(key_bytes("C-c").unwrap(), [3]);
        assert_eq!(key_bytes("C-u").unwrap(), [21]);
        assert_eq!(key_bytes("q").unwrap(), b"q");
        assert!(key_bytes("Hyper").is_err());
    }

    #[test]
    fn paste_is_bracketed_and_sent() {
        assert_eq!(paste("hi\n"), b"\x1b[200~hi\x1b[201~\r");
        assert_eq!(paste("a\x1b[201~b"), b"\x1b[200~ab\x1b[201~\r");
    }
}
