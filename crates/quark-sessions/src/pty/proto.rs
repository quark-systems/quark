//! The `quark-ptyd` wire protocol: one JSON object per line over a Unix
//! socket.
//!
//! A connection sends a [`Request`] and reads one [`Response`], as many
//! times as it likes. After a successful `attach` the connection carries
//! only [`Frame`]s, one per output chunk, until the session ends or either
//! side closes. Bytes travel as base64.

use base64::Engine as _;
use quark_core::session::{Output, SessionId, SessionInfo, SessionSpec, Snapshot, TermSize};
use quark_core::CoreError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Create { spec: SessionSpec },
    Attach { id: SessionId },
    Input { id: SessionId, data: String },
    Resize { id: SessionId, size: TermSize },
    Snapshot { id: SessionId },
    Kill { id: SessionId },
    List,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    Done,
    Session(SessionInfo),
    Sessions(Vec<SessionInfo>),
    Snapshot { size: TermSize, data: String },
    Error { kind: ErrorKind, message: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Invalid,
    NotFound,
    IllegalTransition,
    Refused,
    Backend,
    Unsupported,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    Bytes { data: String },
    Exited { code: Option<i32> },
}

pub fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn decode(s: &str) -> Result<Vec<u8>, CoreError> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| CoreError::Invalid(format!("bad base64: {e}")))
}

impl From<CoreError> for Response {
    fn from(e: CoreError) -> Self {
        let (kind, message) = match e {
            CoreError::Invalid(m) => (ErrorKind::Invalid, m),
            CoreError::NotFound(m) => (ErrorKind::NotFound, m),
            CoreError::IllegalTransition(m) => (ErrorKind::IllegalTransition, m),
            CoreError::Refused(m) => (ErrorKind::Refused, m),
            CoreError::Backend(m) => (ErrorKind::Backend, m),
            CoreError::Unsupported(m) => (ErrorKind::Unsupported, m),
        };
        Response::Error { kind, message }
    }
}

impl From<Snapshot> for Response {
    fn from(s: Snapshot) -> Self {
        Response::Snapshot {
            size: s.size,
            data: encode(&s.bytes),
        }
    }
}

/// The error a [`Response::Error`] carries.
pub fn error(kind: ErrorKind, message: String) -> CoreError {
    match kind {
        ErrorKind::Invalid => CoreError::Invalid(message),
        ErrorKind::NotFound => CoreError::NotFound(message),
        ErrorKind::IllegalTransition => CoreError::IllegalTransition(message),
        ErrorKind::Refused => CoreError::Refused(message),
        ErrorKind::Backend => CoreError::Backend(message),
        ErrorKind::Unsupported => CoreError::Unsupported(message),
    }
}

impl From<Output> for Frame {
    fn from(o: Output) -> Self {
        match o {
            Output::Bytes { data } => Frame::Bytes {
                data: encode(&data),
            },
            Output::Exited { code } => Frame::Exited { code },
        }
    }
}

impl Frame {
    pub fn into_output(self) -> Result<Output, CoreError> {
        Ok(match self {
            Frame::Bytes { data } => Output::Bytes {
                data: decode(&data)?,
            },
            Frame::Exited { code } => Output::Exited { code },
        })
    }
}
