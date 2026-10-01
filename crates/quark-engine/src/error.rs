use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Adapter failures. Each one is surfaced to the caller; the adapter never retries.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("script {script} with args {args:?} is not on the read allowlist")]
    NotAllowed { script: String, args: Vec<String> },

    #[error("{path} is not a genuine engine script: {reason}")]
    NotGenuine { path: PathBuf, reason: String },

    #[error("failed to start {script}: {source}")]
    Spawn {
        script: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{script} timed out after {seconds}s")]
    Timeout { script: String, seconds: u64 },

    #[error("{script} exited with {exit_code:?}: {stderr}")]
    ScriptFailed {
        script: String,
        exit_code: Option<i32>,
        stderr: String,
    },

    #[error("invalid task id {0:?}")]
    InvalidTaskId(String),

    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing {what}: {source}")]
    Json {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("{what} has schema {found:?}, expected {expected}")]
    Schema {
        what: &'static str,
        found: Option<String>,
        expected: &'static str,
    },

    #[error("malformed {what}: {detail}")]
    Malformed { what: &'static str, detail: String },
}
