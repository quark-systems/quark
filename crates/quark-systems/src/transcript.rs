//! Coordinator and worker transcripts.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Who or what produced a transcript entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    /// Input from a person, or from the engine on a person's behalf.
    User,
    /// Text the agent wrote for the reader.
    Assistant,
    /// The agent's visible reasoning, when the harness records it.
    Thinking,
    /// A tool the agent invoked; `text` holds its input.
    ToolCall,
    /// What a tool returned; `text` holds its output.
    ToolResult,
    /// The harness compacted the conversation here: what came before is now
    /// a summary. `text` says who compacted it and at what size.
    Compaction,
}

/// One entry of a coordinator or worker transcript, parsed from the harness's
/// own session log. Payload of `coordinator.message` and `worker.transcript`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TranscriptEntry {
    pub role: TranscriptRole,
    /// Markdown for `user`, `assistant` and `thinking`; tool input or output
    /// otherwise.
    pub text: String,
    /// Tool name, for `tool_call` and `tool_result` when the harness records it.
    pub tool_name: Option<String>,
    /// Pairs a `tool_result` with its `tool_call`.
    pub tool_call_id: Option<String>,
    /// `true` when a tool reported failure.
    pub is_error: bool,
    /// `true` when `text` was cut to the daemon's size limit.
    pub truncated: bool,
    /// RFC 3339 timestamp recorded by the harness, when present.
    pub ts: Option<String>,
    /// What a `tool_call` does, in a form the UI can show without parsing the
    /// harness's tool input. Absent on other roles, and on entries recorded
    /// before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolInfo>,
}

/// What kind of work a tool call does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Reads a file.
    Read,
    /// Changes an existing file.
    Edit,
    /// Creates or overwrites a file.
    Write,
    /// Runs a shell command.
    Shell,
    /// Searches file contents or names, or lists a directory.
    Search,
    /// Fetches a URL or searches the web.
    Web,
    /// Starts a subagent.
    Agent,
    /// Updates the agent's plan or todo list.
    Plan,
    /// Anything else, including MCP tools.
    Other,
}

/// A readable summary of one tool call, derived from its input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ToolInfo {
    pub kind: ToolKind,
    /// One line for the reader, such as `Read src/parser.rs` or
    /// `Run cargo test`.
    pub title: String,
    /// The file the call reads or changes; the first one when it changes
    /// several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The full shell command, for `shell`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The search pattern or web query, for `search` and `web`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Lines added, for `edit` and `write`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<u32>,
    /// Lines removed, for `edit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u32>,
    /// The first lines of the change, for `edit` and `write`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diff: Vec<ToolDiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolDiffLineKind {
    Add,
    Del,
    Context,
}

/// One line of a [`ToolInfo`] diff preview.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ToolDiffLine {
    pub kind: ToolDiffLineKind,
    pub text: String,
}

/// A transcript entry as listed by the history endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TranscriptItem {
    /// The `seq` of the event that carried this entry; pass it as `after` to
    /// page, and use it to merge history with live events.
    pub id: i64,
    #[serde(flatten)]
    pub entry: TranscriptEntry,
}

/// A message for a coordinator, typed into its session as if at the keyboard.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CoordinatorMessage {
    pub text: String,
}

/// The coordinator's session took the message. Its reply arrives as
/// `coordinator.message` events, read from the session log.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CoordinatorMessageAccepted {
    pub coordinator_id: String,
    /// `false` when the text was typed and submitted but the session did not
    /// confirm the submit; check the transcript before sending again.
    pub confirmed: bool,
    pub accepted_at: String,
    /// The command that ran, when the message was one (or a command was
    /// asked for): a catch up with nothing to recap from runs as a status
    /// report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CoordinatorCommand>,
}

/// A request the user makes of a coordinator with one control: a button in
/// the app, or a slash command typed as the whole message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoordinatorCommand {
    /// Recap what happened since the user's last message, then go through
    /// the open decisions one at a time.
    CatchUp,
    /// The Project's state in four sections: needs you, recently landed,
    /// underway, next.
    StatusReport,
    /// Save what the coordinator knows only from its conversation (memory,
    /// unfiled work) before its context is compacted.
    WrapUp,
}

impl CoordinatorCommand {
    pub const ALL: [CoordinatorCommand; 3] = [Self::CatchUp, Self::StatusReport, Self::WrapUp];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CatchUp => "catch_up",
            Self::StatusReport => "status_report",
            Self::WrapUp => "wrap_up",
        }
    }

    /// The slash commands that run it: its own names, then the names
    /// firstmate's users already type (`/ahoy`, `/bearings`, `/stow`).
    pub fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::CatchUp => &["catchup", "catch-up", "ahoy"],
            Self::StatusReport => &["status", "status-report", "bearings"],
            Self::WrapUp => &["wrapup", "wrap-up", "stow"],
        }
    }

    /// The command a message is, when the whole message is one of the
    /// slash commands (case and surrounding space ignored).
    pub fn from_slash(text: &str) -> Option<Self> {
        let word = text.trim().strip_prefix('/')?.to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|c| c.aliases().contains(&word.as_str()))
    }
}

/// Run a [`CoordinatorCommand`].
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CoordinatorCommandRequest {
    pub command: CoordinatorCommand,
}

#[cfg(test)]
mod command_tests {
    use super::CoordinatorCommand;

    #[test]
    fn a_whole_message_slash_command_is_a_command() {
        assert_eq!(
            CoordinatorCommand::from_slash(" /Ahoy "),
            Some(CoordinatorCommand::CatchUp)
        );
        assert_eq!(
            CoordinatorCommand::from_slash("/bearings"),
            Some(CoordinatorCommand::StatusReport)
        );
        assert_eq!(
            CoordinatorCommand::from_slash("/wrap-up"),
            Some(CoordinatorCommand::WrapUp)
        );
        assert_eq!(CoordinatorCommand::from_slash("/stow now"), None);
        assert_eq!(CoordinatorCommand::from_slash("stow"), None);
        assert_eq!(CoordinatorCommand::from_slash("/compact"), None);
    }
}
