//! Chat input to coordinators.
//!
//! A message for a coordinator is typed into its live session the way
//! firstmate's safe path does it: check the composer is empty, type, submit,
//! and confirm the submit. That needs the terminal sessions the daemon
//! attaches to, so delivery sits behind [`CoordinatorInput`]. Until a session
//! client is attached the daemon runs with [`NoSessions`], which refuses every
//! message rather than dropping it. The reply is not returned here: it arrives
//! as `coordinator.message` events read from the coordinator's session log.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::engine::WorkspaceRef;

/// Longest message accepted, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024;

/// How far a delivery got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Typed and the submit was confirmed.
    Confirmed,
    /// Typed and submitted, but the session did not confirm the submit. The
    /// caller must not retype it blindly.
    Unconfirmed,
}

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    /// No live session to type into; nothing was typed.
    #[error("coordinator session unavailable: {0}")]
    Unavailable(String),
    /// Delivery failed; nothing may be assumed delivered.
    #[error("delivery failed: {0}")]
    Failed(String),
}

/// Types a message into the coordinator session of one workspace.
#[async_trait]
pub trait CoordinatorInput: Send + Sync {
    async fn send(&self, ws: &WorkspaceRef, text: &str) -> Result<Delivery, ChatError>;
}

/// The input used while no terminal session client is attached.
#[derive(Debug, Default)]
pub struct NoSessions;

#[async_trait]
impl CoordinatorInput for NoSessions {
    async fn send(&self, _ws: &WorkspaceRef, _text: &str) -> Result<Delivery, ChatError> {
        Err(ChatError::Unavailable(
            "no terminal session client is attached to this daemon".into(),
        ))
    }
}

/// Records messages instead of typing them, for tests and the stub engine.
#[derive(Debug)]
pub struct RecordingInput {
    outcome: Mutex<Result<Delivery, String>>,
    sent: Mutex<Vec<(WorkspaceRef, String)>>,
}

impl Default for RecordingInput {
    fn default() -> Self {
        Self {
            outcome: Mutex::new(Ok(Delivery::Confirmed)),
            sent: Mutex::new(Vec::new()),
        }
    }
}

impl RecordingInput {
    pub fn new() -> Self {
        Self::default()
    }

    /// What every later `send` reports; `Err` makes it fail.
    pub fn set_outcome(&self, outcome: Result<Delivery, String>) {
        *self.outcome.lock().unwrap() = outcome;
    }

    pub fn sent(&self) -> Vec<(WorkspaceRef, String)> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl CoordinatorInput for RecordingInput {
    async fn send(&self, ws: &WorkspaceRef, text: &str) -> Result<Delivery, ChatError> {
        let outcome = self.outcome.lock().unwrap().clone();
        match outcome {
            Ok(d) => {
                self.sent
                    .lock()
                    .unwrap()
                    .push((ws.clone(), text.to_string()));
                Ok(d)
            }
            Err(e) => Err(ChatError::Failed(e)),
        }
    }
}

/// Why `text` cannot be typed into a session, if it cannot. Newlines and tabs
/// are allowed; other control characters (escape sequences in particular)
/// would drive the terminal rather than say something to the coordinator.
pub fn validate(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("text must not be empty".into());
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(format!("text is longer than {MAX_MESSAGE_BYTES} bytes"));
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("text must not contain control characters other than newline and tab".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate;

    #[test]
    fn validates_typed_text() {
        assert!(validate("fix #42\nand add tests").is_ok());
        assert!(validate("  ").is_err());
        assert!(validate("\u{1b}[2J").is_err());
        assert!(validate(&"x".repeat(super::MAX_MESSAGE_BYTES + 1)).is_err());
    }
}
