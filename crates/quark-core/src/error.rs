//! The error every contract returns.

/// Errors shared by the contracts. Implementations map their own failures
/// onto these so callers can match without knowing the subsystem.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    /// The request was refused before anything ran.
    #[error("invalid request: {0}")]
    Invalid(String),
    /// A named thing (task, worktree, session, host, harness) does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// A state transition the task machine does not allow.
    #[error("illegal transition: {0}")]
    IllegalTransition(String),
    /// A guard said no (primary checkout, unlanded work, red main, stale
    /// head). The message names the reason for a person.
    #[error("refused: {0}")]
    Refused(String),
    /// The backing system failed (IO, a subprocess, a database).
    #[error("backend failed: {0}")]
    Backend(String),
    /// The operation is not supported by this implementation.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

pub type Result<T, E = CoreError> = std::result::Result<T, E>;
