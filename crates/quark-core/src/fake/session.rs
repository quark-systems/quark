use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::session::{
    Output, OutputStream, SessionBackend, SessionId, SessionInfo, SessionSpec, Snapshot, TermSize,
};
use crate::{CoreError, Result};

/// One fake terminal: input is echoed back as output.
#[derive(Debug)]
pub struct Pty {
    pub info: SessionInfo,
    pub size: TermSize,
    pub screen: Vec<u8>,
    tx: broadcast::Sender<Output>,
}

/// A [`SessionBackend`] whose sessions echo their input. Nothing runs.
#[derive(Debug, Default, Clone)]
pub struct FakeSessions {
    sessions: Arc<Mutex<BTreeMap<SessionId, Pty>>>,
}

impl FakeSessions {
    pub fn new() -> Self {
        Self::default()
    }

    fn with<T>(&self, id: &SessionId, f: impl FnOnce(&mut Pty) -> T) -> Result<T> {
        let mut s = self.sessions.lock().unwrap();
        let pty = s
            .get_mut(id)
            .ok_or_else(|| CoreError::NotFound(format!("session {}", id.0)))?;
        Ok(f(pty))
    }
}

struct Rx(broadcast::Receiver<Output>);

#[async_trait]
impl OutputStream for Rx {
    async fn next(&mut self) -> Option<Output> {
        loop {
            match self.0.recv().await {
                Ok(o) => return Some(o),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

#[async_trait]
impl SessionBackend for FakeSessions {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn create(&self, spec: &SessionSpec) -> Result<SessionInfo> {
        let mut s = self.sessions.lock().unwrap();
        let id = SessionId(format!("s{}", s.len() + 1));
        let info = SessionInfo {
            id: id.clone(),
            name: spec.name.clone(),
            task: spec.task.clone(),
            alive: true,
            exit_code: None,
        };
        let (tx, _) = broadcast::channel(64);
        s.insert(
            id,
            Pty {
                info: info.clone(),
                size: spec.size,
                screen: Vec::new(),
                tx,
            },
        );
        Ok(info)
    }

    async fn attach(&self, id: &SessionId) -> Result<Box<dyn OutputStream>> {
        self.with(id, |p| {
            Box::new(Rx(p.tx.subscribe())) as Box<dyn OutputStream>
        })
    }

    async fn input(&self, id: &SessionId, bytes: &[u8]) -> Result<()> {
        self.with(id, |p| {
            if !p.info.alive {
                return Err(CoreError::Refused("session has exited".into()));
            }
            p.screen.extend_from_slice(bytes);
            let _ = p.tx.send(Output::Bytes {
                data: bytes.to_vec(),
            });
            Ok(())
        })?
    }

    async fn resize(&self, id: &SessionId, size: TermSize) -> Result<()> {
        self.with(id, |p| p.size = size)
    }

    async fn snapshot(&self, id: &SessionId) -> Result<Snapshot> {
        self.with(id, |p| Snapshot {
            size: p.size,
            bytes: p.screen.clone(),
        })
    }

    async fn kill(&self, id: &SessionId) -> Result<()> {
        self.with(id, |p| {
            p.info.alive = false;
            let _ = p.tx.send(Output::Exited { code: None });
        })
    }

    async fn list(&self) -> Result<Vec<SessionInfo>> {
        Ok(self
            .sessions
            .lock()
            .unwrap()
            .values()
            .map(|p| p.info.clone())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn echoes_input_and_snapshots() {
        let b = FakeSessions::new();
        let s = b
            .create(&SessionSpec {
                task: None,
                name: "w".into(),
                cwd: "/".into(),
                argv: vec!["sh".into()],
                env: Default::default(),
                size: TermSize::default(),
            })
            .await
            .unwrap();
        let mut out = b.attach(&s.id).await.unwrap();
        b.input(&s.id, b"hi").await.unwrap();
        assert_eq!(
            out.next().await,
            Some(Output::Bytes {
                data: b"hi".to_vec()
            })
        );
        assert_eq!(b.snapshot(&s.id).await.unwrap().bytes, b"hi");
        b.kill(&s.id).await.unwrap();
        assert_eq!(out.next().await, Some(Output::Exited { code: None }));
        assert!(b.input(&s.id, b"x").await.is_err());
    }
}
