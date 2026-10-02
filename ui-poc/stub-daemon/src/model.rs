//! Wire types. Field names match `ui-poc/CONTRACT.md` exactly.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub repo: String,
    /// Computed on read: tasks not in `done` or `failed`.
    pub active_tasks: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    NeedsDecision,
    Review,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub state: TaskState,
    pub harness: String,
    pub branch: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionOption {
    pub label: String,
    pub consequence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionState {
    Open,
    Answered,
}

#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub id: String,
    pub project_id: String,
    pub task_id: String,
    pub question: String,
    pub context: String,
    pub options: Vec<DecisionOption>,
    pub recommended: usize,
    pub state: DecisionState,
    pub answer: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Draft,
    Open,
    Merged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Checks {
    Pending,
    Passing,
    Failing,
}

#[derive(Debug, Clone, Serialize)]
pub struct PullRequest {
    pub id: String,
    pub project_id: String,
    pub task_id: String,
    pub number: u32,
    pub title: String,
    pub url: String,
    pub state: PrState,
    pub checks: Checks,
    pub additions: u32,
    pub deletions: u32,
    /// "low" | "medium" | "high"
    pub risk: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comment {
    pub id: String,
    pub path: String,
    pub line: u32,
    pub body: String,
    pub author: String,
    pub ts: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Coordinator,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub id: String,
    pub role: Role,
    pub text: String,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Worker {
    pub id: String,
    pub task_id: String,
    pub title: String,
    pub cols: u16,
    pub rows: u16,
}

/// Current time as an RFC 3339 UTC string with millisecond precision.
pub fn now_ts() -> String {
    ts_ago(0)
}

/// RFC 3339 UTC timestamp `secs` seconds in the past.
pub fn ts_ago(secs: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
