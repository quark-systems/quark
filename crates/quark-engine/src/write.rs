//! Typed engine writes (spec ADR-8).
//!
//! Each [`WriteOp`] names one allowlisted script and renders its own argument
//! vector after validating every field. Caller text only ever lands in a
//! position the script reads as data: the message operand of `fm-send.sh`, the
//! `--opt=value` form of `fm-control.sh`, whose parser never treats such a
//! value as another option, or an environment variable (`fm-home-seed.sh`'s
//! charter and scope).
//!
//! Project creation (J2) adds three operations, all run against the
//! command-center workspace: [`WriteOp::ProjectAdd`] clones a repo into it,
//! [`WriteOp::HomeSeed`] seeds the Project workspace as a local secondmate with
//! those repos, and [`WriteOp::SpawnSecondmate`] starts its coordinator.
//!
//! Decisions (J5) add two: [`WriteOp::Answer`] answers a worker's open keyed
//! decision through `fm-send.sh --resolve-key`, and [`WriteOp::AnswerHold`]
//! answers a captain hold through `fm-captain-hold.sh answer`. Both record who
//! answered (`--answered-by`, from the quark-systems firstmate fork).
//!
//! Verification gates (ADR-15) add [`WriteOp::GatesConfig`], which replaces a
//! workspace's gate config through `fm-gates.sh config-set`.
//!
//! Dispatch profiles (ADR-11) add [`WriteOp::CrewDispatchConfig`], which
//! replaces a workspace's `config/crew-dispatch.json` through
//! `fm-crew-dispatch.sh config-set`.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::{validate_task_id, Error, Result};

pub const SEND: &str = "fm-send.sh";
pub const CONTROL: &str = "fm-control.sh";
pub const PROJECT_ADD: &str = "fm-project-add.sh";
pub const HOME_SEED: &str = "fm-home-seed.sh";
pub const SPAWN: &str = "fm-spawn.sh";
pub const PR_MERGE: &str = "fm-pr-merge.sh";
pub const PROJECT_YOLO: &str = "fm-project-yolo.sh";
pub const CAPTAIN_HOLD: &str = "fm-captain-hold.sh";
pub const GATES: &str = "fm-gates.sh";
pub const INBOX: &str = "fm-inbox.sh";
pub const CREW_DISPATCH: &str = "fm-crew-dispatch.sh";

/// Largest gate config accepted, in bytes (`fm-gates.sh` refuses more).
pub const MAX_GATES_CONFIG_BYTES: usize = 256 * 1024;

/// The target name [`WriteOp::GatesConfig`] reports: it changes the
/// workspace's config, not one task.
pub const GATES_TARGET: &str = "gates";

/// Largest crew dispatch config accepted, in bytes (`fm-crew-dispatch.sh`
/// refuses more).
pub const MAX_CREW_DISPATCH_CONFIG_BYTES: usize = 256 * 1024;

/// The target name [`WriteOp::CrewDispatchConfig`] reports.
pub const CREW_DISPATCH_TARGET: &str = "crew-dispatch";
/// The target [`WriteOp::InboxNote`] names: the workspace's own inbox.
pub const INBOX_TARGET: &str = "inbox";

/// Longest scope or project description accepted, in characters.
pub const MAX_LINE_CHARS: usize = 600;

/// Longest secondmate charter accepted, in characters. The charter is the
/// coordinator's standing job description, so it is longer than a one-line
/// summary; `fm-home-seed.sh` itself sets no limit.
pub const MAX_CHARTER_CHARS: usize = 8192;

/// Largest steering message accepted, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Largest relaunch note accepted, in bytes.
pub const MAX_NOTE_BYTES: usize = 16 * 1024;

/// Longest harness, model or effort token accepted.
const MAX_TOKEN_LEN: usize = 128;

/// Largest answer the engine records for a captain hold, in bytes.
pub const MAX_HOLD_ANSWER_BYTES: usize = 8192;

/// Longest answering-user identity the engine accepts, in bytes.
pub const MAX_ANSWERED_BY_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOp {
    /// Steer a task through its durable inbox: `fm-send.sh <task> <text>`.
    Send { task_id: String, text: String },
    /// Answer a task's open keyed decision and close it in the same act:
    /// `fm-send.sh <task> --resolve-key=<key> --answered-by=<user> <text>`.
    Answer {
        task_id: String,
        key: String,
        text: String,
        answered_by: String,
    },
    /// Answer a backlog task held for the captain, recording the answer from
    /// `decision_file` (which the caller writes and removes):
    /// `fm-captain-hold.sh answer <task> --decision-file <path> [--release]
    /// --answered-by <user>`. `release` lifts the hold so held work resumes
    /// instead of closing the task.
    AnswerHold {
        task_id: String,
        decision_file: PathBuf,
        release: bool,
        answered_by: String,
    },
    /// Stop a task's agent, keeping its terminal, worktree and every
    /// uncommitted change: `fm-control.sh <task> exit`.
    Exit { task_id: String },
    /// Replace a task's agent in the same terminal and worktree, optionally on
    /// another harness, model or effort: `fm-control.sh <task> relaunch`.
    Relaunch {
        task_id: String,
        harness: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        /// Carried to the new agent, which inherits the worktree but none of
        /// the conversation.
        note: String,
    },
    /// Clone a repo into this workspace and register it:
    /// `fm-project-add.sh <name> <origin> --mode <mode> --desc <text>`.
    /// Idempotent for the same name and origin.
    ProjectAdd {
        name: String,
        origin: String,
        mode: DeliveryMode,
        description: String,
    },
    /// Seed a secondmate workspace at `home` with already-added projects:
    /// `fm-home-seed.sh <id> <home> <project>...`, with the charter and its
    /// routing scope in `FM_SECONDMATE_CHARTER` / `FM_SECONDMATE_SCOPE`.
    HomeSeed {
        id: String,
        home: PathBuf,
        projects: Vec<String>,
        charter: String,
        scope: String,
    },
    /// Launch a seeded secondmate's agent:
    /// `fm-spawn.sh <id> <home> --harness <h> [--model <m>] [--effort <e>] --secondmate`.
    SpawnSecondmate {
        id: String,
        home: PathBuf,
        harness: String,
        model: Option<String>,
        effort: Option<String>,
    },
    /// Merge a task's pull request through the engine's guarded merge, which
    /// re-reads it live and refuses unless it is green and mergeable:
    /// `fm-pr-merge.sh <task> <url> [-- --<method>]`.
    PrMerge {
        task_id: String,
        url: String,
        method: Option<MergeMethod>,
    },
    /// Set a registered project's standing merge posture (yolo), keeping its
    /// delivery mode: `fm-project-yolo.sh <name> <on|off>`.
    ProjectYolo { name: String, on: bool },
    /// Replace the workspace's verification-gate config (`config/gates.json`,
    /// schema `fm.gates.v1`): `fm-gates.sh config-set` with the JSON in
    /// `FM_GATES_CONFIG_JSON`. The script validates it before writing.
    GatesConfig { json: String },
    /// Replace the workspace's crew dispatch profiles
    /// (`config/crew-dispatch.json`): `fm-crew-dispatch.sh config-set` with the
    /// JSON in `FM_CREW_DISPATCH_CONFIG_JSON`. The script validates it and
    /// exits 1, leaving the old file in place, when it is invalid.
    CrewDispatchConfig { json: String },
    /// Leave the user's note in the workspace inbox, waking the coordinator
    /// at its next check: `fm-inbox.sh note <text>`.
    InboxNote { text: String },
}

/// How a pull request is merged on GitHub. GitLab uses the project's setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

impl MergeMethod {
    pub fn flag(self) -> &'static str {
        match self {
            MergeMethod::Squash => "--squash",
            MergeMethod::Merge => "--merge",
            MergeMethod::Rebase => "--rebase",
        }
    }
}

/// How a project's changes reach its default branch, as registered with the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    /// The engine's validation pipeline runs before a PR is opened.
    NoMistakes,
    /// The worker pushes and opens a PR without the pipeline.
    DirectPr,
}

impl DeliveryMode {
    pub fn as_engine_str(self) -> &'static str {
        match self {
            DeliveryMode::NoMistakes => "no-mistakes",
            DeliveryMode::DirectPr => "direct-PR",
        }
    }
}

impl WriteOp {
    pub fn script(&self) -> &'static str {
        match self {
            WriteOp::Send { .. } | WriteOp::Answer { .. } => SEND,
            WriteOp::AnswerHold { .. } => CAPTAIN_HOLD,
            WriteOp::Exit { .. } | WriteOp::Relaunch { .. } => CONTROL,
            WriteOp::ProjectAdd { .. } => PROJECT_ADD,
            WriteOp::HomeSeed { .. } => HOME_SEED,
            WriteOp::SpawnSecondmate { .. } => SPAWN,
            WriteOp::PrMerge { .. } => PR_MERGE,
            WriteOp::ProjectYolo { .. } => PROJECT_YOLO,
            WriteOp::GatesConfig { .. } => GATES,
            WriteOp::CrewDispatchConfig { .. } => CREW_DISPATCH,
            WriteOp::InboxNote { .. } => INBOX,
        }
    }

    /// The engine id the operation targets: a task, a project name or a
    /// secondmate id. Every one follows the engine's path-safe id rule.
    pub fn task_id(&self) -> &str {
        match self {
            WriteOp::Send { task_id, .. }
            | WriteOp::Answer { task_id, .. }
            | WriteOp::AnswerHold { task_id, .. }
            | WriteOp::Exit { task_id }
            | WriteOp::Relaunch { task_id, .. }
            | WriteOp::PrMerge { task_id, .. } => task_id,
            WriteOp::ProjectAdd { name, .. } | WriteOp::ProjectYolo { name, .. } => name,
            WriteOp::HomeSeed { id, .. } | WriteOp::SpawnSecondmate { id, .. } => id,
            WriteOp::GatesConfig { .. } => GATES_TARGET,
            WriteOp::CrewDispatchConfig { .. } => CREW_DISPATCH_TARGET,
            WriteOp::InboxNote { .. } => INBOX_TARGET,
        }
    }

    /// Default bound for the call. `fm-control.sh` waits on verified
    /// postconditions (up to 30s for an exit, 90s more for a launch). Clones
    /// and `no-mistakes init` can take minutes on a large repo.
    pub fn timeout(&self) -> Duration {
        match self {
            WriteOp::Send { .. }
            | WriteOp::ProjectYolo { .. }
            | WriteOp::Answer { .. }
            | WriteOp::AnswerHold { .. }
            | WriteOp::GatesConfig { .. }
            | WriteOp::CrewDispatchConfig { .. }
            | WriteOp::InboxNote { .. } => Duration::from_secs(60),
            WriteOp::PrMerge { .. } => Duration::from_secs(300),
            WriteOp::Exit { .. } => Duration::from_secs(120),
            WriteOp::Relaunch { .. } | WriteOp::SpawnSecondmate { .. } => Duration::from_secs(300),
            WriteOp::ProjectAdd { .. } | WriteOp::HomeSeed { .. } => Duration::from_secs(15 * 60),
        }
    }

    /// Environment the script runs with besides `FM_HOME`. Only call after
    /// [`WriteOp::argv`] has validated the operation.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        match self {
            WriteOp::HomeSeed { charter, scope, .. } => vec![
                ("FM_SECONDMATE_CHARTER", one_line(charter)),
                ("FM_SECONDMATE_SCOPE", one_line(scope)),
            ],
            WriteOp::GatesConfig { json } => vec![("FM_GATES_CONFIG_JSON", json.clone())],
            WriteOp::CrewDispatchConfig { json } => {
                vec![("FM_CREW_DISPATCH_CONFIG_JSON", json.clone())]
            }
            _ => Vec::new(),
        }
    }

    /// Validate every field and render the script's argument vector.
    pub fn argv(&self) -> Result<Vec<String>> {
        let script = self.script();
        let invalid = |reason: String| Error::InvalidArgument { script, reason };
        validate_task_id(self.task_id())?;
        // Both scripts read their first operand as the target, but
        // fm-control.sh answers `-h`/`--help` there with usage and exit 0.
        if self.task_id().starts_with('-') {
            return Err(Error::InvalidTaskId(self.task_id().to_string()));
        }
        match self {
            WriteOp::Send { task_id, text } => {
                check_message(text).map_err(invalid)?;
                Ok(vec![task_id.clone(), text.clone()])
            }
            WriteOp::Answer {
                task_id,
                key,
                text,
                answered_by,
            } => {
                check_decision_key(key).map_err(invalid)?;
                check_answered_by(answered_by).map_err(invalid)?;
                check_message(text).map_err(invalid)?;
                Ok(vec![
                    task_id.clone(),
                    format!("--resolve-key={key}"),
                    format!("--answered-by={answered_by}"),
                    text.clone(),
                ])
            }
            WriteOp::AnswerHold {
                task_id,
                decision_file,
                release,
                answered_by,
            } => {
                check_answered_by(answered_by).map_err(invalid)?;
                if !decision_file.is_absolute() {
                    return Err(invalid("the decision file path must be absolute".into()));
                }
                let path = decision_file
                    .to_str()
                    .ok_or_else(|| invalid("the decision file path is not UTF-8".into()))?;
                let mut argv = vec![
                    "answer".into(),
                    task_id.clone(),
                    "--decision-file".into(),
                    path.to_string(),
                ];
                if *release {
                    argv.push("--release".into());
                }
                argv.extend(["--answered-by".into(), answered_by.clone()]);
                Ok(argv)
            }
            WriteOp::Exit { task_id } => Ok(vec![task_id.clone(), "exit".into()]),
            WriteOp::Relaunch {
                task_id,
                harness,
                model,
                effort,
                note,
            } => {
                let mut argv = vec![task_id.clone(), "relaunch".into()];
                for (flag, value) in [("harness", harness), ("model", model), ("effort", effort)] {
                    if let Some(v) = value {
                        check_token(flag, v).map_err(invalid)?;
                        argv.push(format!("--{flag}={v}"));
                    }
                }
                check_text("note", note, MAX_NOTE_BYTES).map_err(invalid)?;
                argv.push(format!("--note={note}"));
                Ok(argv)
            }
            WriteOp::ProjectAdd {
                name,
                origin,
                mode,
                description,
            } => {
                check_origin(origin).map_err(invalid)?;
                let description = check_line("description", description).map_err(invalid)?;
                Ok(vec![
                    name.clone(),
                    origin.clone(),
                    "--mode".into(),
                    mode.as_engine_str().into(),
                    "--desc".into(),
                    description,
                ])
            }
            WriteOp::HomeSeed {
                id,
                home,
                projects,
                charter,
                scope,
            } => {
                let home = check_home(home).map_err(invalid)?;
                if projects.is_empty() {
                    return Err(invalid("at least one project is required".into()));
                }
                for p in projects {
                    validate_task_id(p)?;
                    if p.starts_with('-') {
                        return Err(Error::InvalidTaskId(p.clone()));
                    }
                }
                let charter =
                    check_line_max("charter", charter, MAX_CHARTER_CHARS).map_err(invalid)?;
                check_line("scope", scope).map_err(invalid)?;
                // fm-home-seed.sh refuses a charter still carrying the brief
                // placeholder; refuse it here so nothing runs.
                if charter.contains("{TASK}") {
                    return Err(invalid("charter may not contain {TASK}".into()));
                }
                let mut argv = vec![id.clone(), home];
                argv.extend(projects.iter().cloned());
                Ok(argv)
            }
            WriteOp::SpawnSecondmate {
                id,
                home,
                harness,
                model,
                effort,
            } => {
                let home = check_home(home).map_err(invalid)?;
                check_token("harness", harness).map_err(invalid)?;
                let mut argv = vec![id.clone(), home, "--harness".into(), harness.clone()];
                if let Some(m) = model {
                    check_token("model", m).map_err(invalid)?;
                    argv.extend(["--model".into(), m.clone()]);
                }
                if let Some(e) = effort {
                    check_token("effort", e).map_err(invalid)?;
                    argv.extend(["--effort".into(), e.clone()]);
                }
                argv.push("--secondmate".into());
                Ok(argv)
            }
            WriteOp::PrMerge {
                task_id,
                url,
                method,
            } => {
                check_pr_url(url).map_err(invalid)?;
                let mut argv = vec![task_id.clone(), url.clone()];
                if let Some(m) = method {
                    argv.extend(["--".into(), m.flag().into()]);
                }
                Ok(argv)
            }
            WriteOp::ProjectYolo { name, on } => {
                Ok(vec![name.clone(), if *on { "on" } else { "off" }.into()])
            }
            WriteOp::InboxNote { text } => {
                check_message(text).map_err(invalid)?;
                // `note -` reads the body from standard input.
                if text.trim() == "-" {
                    return Err(invalid("a note may not be just \"-\"".into()));
                }
                Ok(vec!["note".into(), text.clone()])
            }
            WriteOp::GatesConfig { json } => {
                check_json_object("gates config", json, MAX_GATES_CONFIG_BYTES).map_err(invalid)?;
                Ok(vec!["config-set".into()])
            }
            WriteOp::CrewDispatchConfig { json } => {
                check_json_object("crew dispatch config", json, MAX_CREW_DISPATCH_CONFIG_BYTES)
                    .map_err(invalid)?;
                Ok(vec!["config-set".into()])
            }
        }
    }
}

/// What `fm-project-add.sh` reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAdded {
    pub name: String,
    pub path: PathBuf,
    /// `false` when the project was already cloned and registered.
    pub created: bool,
}

/// Parse `project=<name> path=<path> ... result=<added|unchanged>`.
pub fn parse_project_added(stdout: &str) -> Result<ProjectAdded> {
    let f = last_fields(PROJECT_ADD, stdout)?;
    Ok(ProjectAdded {
        name: field(PROJECT_ADD, &f, "project")?.to_string(),
        path: PathBuf::from(field(PROJECT_ADD, &f, "path")?),
        created: field(PROJECT_ADD, &f, "result")? == "added",
    })
}

/// Parse `fm-home-seed.sh`'s final `home=<path>` line.
pub fn parse_seeded_home(stdout: &str) -> Result<PathBuf> {
    let f = last_fields(HOME_SEED, stdout)?;
    Ok(PathBuf::from(field(HOME_SEED, &f, "home")?))
}

/// The `spawned <id> ...` line `fm-spawn.sh` prints on success.
pub fn parse_spawned(stdout: &str) -> Result<String> {
    stdout
        .lines()
        .rev()
        .find(|l| l.starts_with("spawned "))
        .map(str::to_string)
        .ok_or_else(|| Error::Malformed {
            what: SPAWN,
            detail: "no `spawned` line".into(),
        })
}

fn last_fields(what: &'static str, stdout: &str) -> Result<Vec<(String, String)>> {
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| Error::Malformed {
            what,
            detail: "no output".into(),
        })?;
    Ok(line
        .split_whitespace()
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect())
}

fn field<'a>(what: &'static str, f: &'a [(String, String)], key: &str) -> Result<&'a str> {
    f.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| Error::Malformed {
            what,
            detail: format!("missing {key}="),
        })
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Collapse whitespace to one line: nonempty, bounded, no control characters.
fn check_line(what: &str, s: &str) -> std::result::Result<String, String> {
    check_line_max(what, s, MAX_LINE_CHARS)
}

fn check_line_max(what: &str, s: &str, max: usize) -> std::result::Result<String, String> {
    let line = one_line(s);
    if line.is_empty() {
        return Err(format!("{what} is empty"));
    }
    if line.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    if line.chars().any(char::is_control) {
        return Err(format!("{what} contains a control character"));
    }
    Ok(line)
}

/// A plain clone URL: https/http/ssh/git/file, scp-like `host:path`, or an
/// absolute path. No whitespace, no option-shaped value, no remote helper.
/// `fm-project-add.sh` re-validates with the engine's own origin rules.
fn check_origin(s: &str) -> std::result::Result<(), String> {
    let bad = |d: &str| Err(format!("origin {s:?} {d}"));
    if s.is_empty() || s.len() > 2048 {
        return bad("must be 1 to 2048 bytes");
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return bad("contains whitespace or a control character");
    }
    if s.starts_with('-') {
        return bad("starts with '-'");
    }
    if s.contains("::") {
        return bad("names a remote helper");
    }
    if let Some((scheme, rest)) = s.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh" | "git" | "file") {
            return bad("uses an unsupported scheme");
        }
        if rest.is_empty() || rest.starts_with('-') {
            return bad("has no host or path");
        }
        return Ok(());
    }
    if s.starts_with('/') {
        return if s.split('/').any(|c| c == "..") {
            bad("contains '..'")
        } else {
            Ok(())
        };
    }
    match s.split_once(':') {
        Some((host, path)) if !host.is_empty() && !path.is_empty() && !host.contains('/') => Ok(()),
        _ => bad("is not a clone URL"),
    }
}

/// An absolute, normalized UTF-8 path without the registry delimiters `;` and `)`.
fn check_home(p: &Path) -> std::result::Result<String, String> {
    if !p.is_absolute() {
        return Err(format!("workspace path {} is not absolute", p.display()));
    }
    if p.components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(format!("workspace path {} is not normalized", p.display()));
    }
    let s = p.to_str().ok_or("workspace path is not UTF-8")?;
    if s.chars().any(|c| c.is_control() || matches!(c, ';' | ')')) {
        return Err(format!(
            "workspace path {s:?} contains ';', ')' or a control character"
        ));
    }
    Ok(s.trim_end_matches('/').to_string())
}

/// A steering message or an answer to a worker: data to `fm-send.sh`.
fn check_message(text: &str) -> std::result::Result<(), String> {
    check_text("message", text, MAX_MESSAGE_BYTES)?;
    // fm-send.sh reads a leading `--` as one of its own flags, and types a
    // leading `/` or `$` straight into the harness as a command instead of
    // recording it in the durable inbox.
    let lead = text.trim_start();
    if lead.starts_with("--") || lead.starts_with('/') || lead.starts_with('$') {
        return Err("a message may not start with \"--\", \"/\" or \"$\"".into());
    }
    Ok(())
}

/// The text of a captain hold answer, which the engine stores whole.
pub fn check_hold_answer(text: &str) -> std::result::Result<(), String> {
    check_text("answer", text, MAX_HOLD_ANSWER_BYTES)
}

/// A decision key as `fm-send.sh --resolve-key` accepts it.
fn check_decision_key(key: &str) -> std::result::Result<(), String> {
    let ok = !key.is_empty()
        && key.len() <= MAX_TOKEN_LEN
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(format!("decision key {key:?} is not valid"))
    }
}

/// The answering user: one line of at most 128 bytes, no control characters.
pub fn check_answered_by(user: &str) -> std::result::Result<(), String> {
    if user.trim().is_empty() {
        return Err("answered_by is empty".into());
    }
    if user.len() > MAX_ANSWERED_BY_BYTES {
        return Err(format!(
            "answered_by is {} bytes; the limit is {MAX_ANSWERED_BY_BYTES}",
            user.len()
        ));
    }
    if user.chars().any(char::is_control) {
        return Err("answered_by must be one line without control characters".into());
    }
    Ok(())
}

/// A config handed to a `config-set` script: one JSON object of at most `max`
/// bytes.
fn check_json_object(what: &str, json: &str, max: usize) -> std::result::Result<(), String> {
    check_text(what, json, max)?;
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("{what} is not JSON: {e}"))?;
    if !v.is_object() {
        return Err(format!("{what} must be a JSON object"));
    }
    Ok(())
}

fn check_text(what: &str, text: &str, max: usize) -> std::result::Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if text.len() > max {
        return Err(format!(
            "{what} is {} bytes; the limit is {max}",
            text.len()
        ));
    }
    if text.contains('\0') {
        return Err(format!("{what} contains a NUL byte"));
    }
    Ok(())
}

/// A pull request URL as `fm-pr-merge.sh` parses it: https, one token.
fn check_pr_url(s: &str) -> std::result::Result<(), String> {
    let ok = s.len() <= 2048
        && s.strip_prefix("https://")
            .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('-'))
        && !s.chars().any(|c| c.is_whitespace() || c.is_control());
    if ok {
        Ok(())
    } else {
        Err(format!("pull request URL {s:?} is not an https URL"))
    }
}

/// What `fm-project-yolo.sh` reported.
pub fn parse_project_yolo(stdout: &str) -> Result<bool> {
    let f = last_fields(PROJECT_YOLO, stdout)?;
    match field(PROJECT_YOLO, &f, "yolo")? {
        "on" => Ok(true),
        "off" => Ok(false),
        other => Err(Error::Malformed {
            what: PROJECT_YOLO,
            detail: format!("yolo={other}"),
        }),
    }
}

/// Harness, model and effort names: `A-Za-z0-9._:/+-`, not starting with `-`.
fn check_token(what: &str, v: &str) -> std::result::Result<(), String> {
    let ok = !v.is_empty()
        && v.len() <= MAX_TOKEN_LEN
        && !v.starts_with('-')
        && v.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'/' | b'+' | b'-')
        });
    if ok {
        Ok(())
    } else {
        Err(format!("{what} {v:?} is not a valid name"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_merge_and_yolo_argv() {
        let op = WriteOp::PrMerge {
            task_id: "ship-x".into(),
            url: "https://github.com/a/b/pull/3".into(),
            method: Some(MergeMethod::Rebase),
        };
        assert_eq!(op.script(), PR_MERGE);
        assert_eq!(
            op.argv().unwrap(),
            ["ship-x", "https://github.com/a/b/pull/3", "--", "--rebase"]
        );
        for url in [
            "http://github.com/a/b/pull/3",
            "https://-x",
            "https://a b",
            "--admin",
        ] {
            let op = WriteOp::PrMerge {
                task_id: "ship-x".into(),
                url: url.into(),
                method: None,
            };
            assert!(op.argv().is_err(), "{url}");
        }
        let op = WriteOp::ProjectYolo {
            name: "quark".into(),
            on: true,
        };
        assert_eq!(op.argv().unwrap(), ["quark", "on"]);
        assert!(WriteOp::ProjectYolo {
            name: "--x".into(),
            on: true
        }
        .argv()
        .is_err());
        assert!(
            parse_project_yolo("project=quark mode=direct-PR yolo=on result=changed\n").unwrap()
        );
    }

    fn send(text: &str) -> WriteOp {
        WriteOp::Send {
            task_id: "t1".into(),
            text: text.into(),
        }
    }

    #[test]
    fn inbox_note_renders_and_refuses_stdin() {
        let note = |t: &str| WriteOp::InboxNote { text: t.into() };
        assert_eq!(note("look at CI").script(), INBOX);
        assert_eq!(
            note("look at CI").argv().unwrap(),
            vec!["note", "look at CI"]
        );
        for bad in ["-", " - ", "", "--x", "/quit"] {
            assert!(note(bad).argv().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn send_renders_task_and_text() {
        assert_eq!(
            send("please add tests\nfor the parser").argv().unwrap(),
            vec!["t1", "please add tests\nfor the parser"]
        );
        // A single leading dash is ordinary text to fm-send.sh.
        assert!(send("- first item").argv().is_ok());
    }

    #[test]
    fn send_refuses_flag_and_command_text() {
        for bad in [
            "",
            "   ",
            "--resolve-key x",
            "--key",
            "/quit",
            "  /compact",
            "$skill",
            "a\0b",
        ] {
            assert!(
                matches!(send(bad).argv(), Err(Error::InvalidArgument { .. })),
                "{bad:?}"
            );
        }
        let big = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert!(send(&big).argv().is_err());
    }

    #[test]
    fn answer_renders_flags_in_equals_form() {
        let op = WriteOp::Answer {
            task_id: "t1".into(),
            key: "api-shape".into(),
            text: "go with REST".into(),
            answered_by: "--Matt S".into(),
        };
        assert_eq!(op.script(), SEND);
        assert_eq!(
            op.argv().unwrap(),
            vec![
                "t1",
                "--resolve-key=api-shape",
                "--answered-by=--Matt S",
                "go with REST"
            ]
        );
        for (key, user, text) in [
            ("bad key", "m", "t"),
            ("", "m", "t"),
            ("k", "", "t"),
            ("k", "a\nb", "t"),
            ("k", "m", "--resolve-key x"),
            ("k", "m", "/quit"),
        ] {
            let op = WriteOp::Answer {
                task_id: "t1".into(),
                key: key.into(),
                text: text.into(),
                answered_by: user.into(),
            };
            assert!(op.argv().is_err(), "{key:?} {user:?} {text:?}");
        }
    }

    #[test]
    fn answer_hold_renders_release_and_answerer() {
        let op = WriteOp::AnswerHold {
            task_id: "pick-db".into(),
            decision_file: "/tmp/d.txt".into(),
            release: true,
            answered_by: "Matt".into(),
        };
        assert_eq!(op.script(), CAPTAIN_HOLD);
        assert_eq!(
            op.argv().unwrap(),
            vec![
                "answer",
                "pick-db",
                "--decision-file",
                "/tmp/d.txt",
                "--release",
                "--answered-by",
                "Matt"
            ]
        );
        let op = WriteOp::AnswerHold {
            task_id: "pick-db".into(),
            decision_file: "d.txt".into(),
            release: false,
            answered_by: "Matt".into(),
        };
        assert!(op.argv().is_err());
        let op = WriteOp::AnswerHold {
            task_id: "pick-db".into(),
            decision_file: "/tmp/d.txt".into(),
            release: false,
            answered_by: "x".repeat(MAX_ANSWERED_BY_BYTES + 1),
        };
        assert!(op.argv().is_err());
    }

    #[test]
    fn task_id_is_validated() {
        let op = WriteOp::Exit {
            task_id: "../x".into(),
        };
        assert!(matches!(op.argv(), Err(Error::InvalidTaskId(_))));
        // fm-control.sh would print usage and exit 0 for this "task".
        let op = WriteOp::Exit {
            task_id: "--help".into(),
        };
        assert!(matches!(op.argv(), Err(Error::InvalidTaskId(_))));
    }

    #[test]
    fn relaunch_uses_equals_form() {
        let op = WriteOp::Relaunch {
            task_id: "t1".into(),
            harness: Some("codex".into()),
            model: Some("openai/gpt-5.6".into()),
            effort: None,
            note: "--carry on from the failing test".into(),
        };
        assert_eq!(
            op.argv().unwrap(),
            vec![
                "t1",
                "relaunch",
                "--harness=codex",
                "--model=openai/gpt-5.6",
                "--note=--carry on from the failing test"
            ]
        );
        assert_eq!(op.script(), CONTROL);
    }

    #[test]
    fn relaunch_refuses_bad_tokens_and_empty_note() {
        for harness in ["", "-x", "a b", "a=b", "a;b"] {
            let op = WriteOp::Relaunch {
                task_id: "t1".into(),
                harness: Some(harness.into()),
                model: None,
                effort: None,
                note: "n".into(),
            };
            assert!(op.argv().is_err(), "{harness:?}");
        }
        let op = WriteOp::Relaunch {
            task_id: "t1".into(),
            harness: None,
            model: None,
            effort: None,
            note: " ".into(),
        };
        assert!(op.argv().is_err());
    }

    #[test]
    fn project_add_renders_mode_and_one_line_description() {
        let op = WriteOp::ProjectAdd {
            name: "quark".into(),
            origin: "https://github.com/quark-systems/quark.git".into(),
            mode: DeliveryMode::DirectPr,
            description: "  the\n daemon ".into(),
        };
        assert_eq!(op.script(), PROJECT_ADD);
        assert_eq!(
            op.argv().unwrap(),
            vec![
                "quark",
                "https://github.com/quark-systems/quark.git",
                "--mode",
                "direct-PR",
                "--desc",
                "the daemon"
            ]
        );
    }

    #[test]
    fn project_add_refuses_unsafe_names_and_origins() {
        let op = |name: &str, origin: &str| WriteOp::ProjectAdd {
            name: name.into(),
            origin: origin.into(),
            mode: DeliveryMode::NoMistakes,
            description: "d".into(),
        };
        for bad in ["", ".x", "a/b", "a b", "--mode"] {
            assert!(op(bad, "/r.git").argv().is_err(), "{bad:?}");
        }
        for bad in [
            "",
            "-uhack",
            "ext::sh -c x",
            "ftp://h/r",
            "relative/path",
            "/a/../b",
            "https://h/r\n",
        ] {
            assert!(op("p", bad).argv().is_err(), "{bad:?}");
        }
        for ok in [
            "https://github.com/o/r.git",
            "git@github.com:o/r.git",
            "ssh://git@h:22/r",
            "file:///srv/r.git",
            "/srv/r.git",
        ] {
            assert!(op("p", ok).argv().is_ok(), "{ok}");
        }
    }

    fn seed(home: &str, projects: &[&str], charter: &str) -> WriteOp {
        WriteOp::HomeSeed {
            id: "prj_1".into(),
            home: home.into(),
            projects: projects.iter().map(|p| p.to_string()).collect(),
            charter: charter.into(),
            scope: "All work.".into(),
        }
    }

    #[test]
    fn home_seed_passes_charter_in_env() {
        let op = seed(
            "/q/workspaces/prj_1/",
            &["a", "b"],
            "Coordinate\nthe Project.",
        );
        assert_eq!(
            op.argv().unwrap(),
            vec!["prj_1", "/q/workspaces/prj_1", "a", "b"]
        );
        assert_eq!(
            op.env(),
            vec![
                (
                    "FM_SECONDMATE_CHARTER",
                    "Coordinate the Project.".to_string()
                ),
                ("FM_SECONDMATE_SCOPE", "All work.".to_string())
            ]
        );
        for bad in [
            seed("rel", &["a"], "c"),
            seed("/q/../p", &["a"], "c"),
            seed("/q/a;b", &["a"], "c"),
            seed("/q/p", &[], "c"),
            seed("/q/p", &["-a"], "c"),
            seed("/q/p", &["a"], " "),
            seed("/q/p", &["a"], "{TASK}"),
            seed("/q/p", &["a"], &"c".repeat(MAX_CHARTER_CHARS + 1)),
        ] {
            assert!(bad.argv().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn spawn_secondmate_ends_with_the_flag() {
        let op = WriteOp::SpawnSecondmate {
            id: "prj_1".into(),
            home: "/q/workspaces/prj_1".into(),
            harness: "codex".into(),
            model: Some("gpt-5.6".into()),
            effort: Some("high".into()),
        };
        assert_eq!(
            op.argv().unwrap(),
            vec![
                "prj_1",
                "/q/workspaces/prj_1",
                "--harness",
                "codex",
                "--model",
                "gpt-5.6",
                "--effort",
                "high",
                "--secondmate"
            ]
        );
        let bad = WriteOp::SpawnSecondmate {
            id: "prj_1".into(),
            home: "/q/p".into(),
            harness: "-x".into(),
            model: None,
            effort: None,
        };
        assert!(bad.argv().is_err());
    }

    #[test]
    fn parses_script_results() {
        let added = parse_project_added(
            "cloning\nproject=r path=/h/projects/r mode=direct-PR yolo=off result=unchanged\n",
        )
        .unwrap();
        assert_eq!(added.path, PathBuf::from("/h/projects/r"));
        assert!(!added.created);
        assert_eq!(
            parse_seeded_home("home=/q/ws/p1\n").unwrap(),
            PathBuf::from("/q/ws/p1")
        );
        assert!(parse_seeded_home("").is_err());
        assert_eq!(
            parse_spawned("x\nspawned p1 harness=claude kind=secondmate\n").unwrap(),
            "spawned p1 harness=claude kind=secondmate"
        );
        assert!(parse_spawned("nothing").is_err());
    }

    #[test]
    fn gates_config_passes_json_in_the_environment() {
        let op = WriteOp::GatesConfig {
            json: r#"{"schema":"fm.gates.v1","repos":{}}"#.into(),
        };
        assert_eq!(op.script(), GATES);
        assert_eq!(op.argv().unwrap(), vec!["config-set".to_string()]);
        assert_eq!(
            op.env(),
            vec![(
                "FM_GATES_CONFIG_JSON",
                r#"{"schema":"fm.gates.v1","repos":{}}"#.to_string()
            )]
        );
        for bad in [
            "",
            "not json",
            "[1]",
            &"x".repeat(MAX_GATES_CONFIG_BYTES + 1),
        ] {
            let op = WriteOp::GatesConfig { json: bad.into() };
            assert!(op.argv().is_err(), "accepted {bad:.20}");
        }
    }

    #[test]
    fn crew_dispatch_config_passes_json_in_the_environment() {
        let json = r#"{"rules":[],"default":{"harness":"claude"}}"#;
        let op = WriteOp::CrewDispatchConfig { json: json.into() };
        assert_eq!(op.script(), CREW_DISPATCH);
        assert_eq!(op.task_id(), CREW_DISPATCH_TARGET);
        assert_eq!(op.argv().unwrap(), vec!["config-set".to_string()]);
        assert_eq!(
            op.env(),
            vec![("FM_CREW_DISPATCH_CONFIG_JSON", json.to_string())]
        );
        for bad in [
            "",
            "not json",
            "[1]",
            &format!(
                "{{\"x\":\"{}\"}}",
                "x".repeat(MAX_CREW_DISPATCH_CONFIG_BYTES)
            ),
        ] {
            let op = WriteOp::CrewDispatchConfig { json: bad.into() };
            assert!(op.argv().is_err(), "accepted {bad:.20}");
        }
    }
}
