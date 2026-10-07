//! The output stream both backends hand to viewers.
//!
//! A backend broadcasts [`Chunk`]s per session. A viewer only starts
//! forwarding at a repaint (a chunk that redraws the whole screen from a
//! reset), so it never sees output it has no screen for. When a viewer falls
//! behind and loses chunks it asks the backend for a fresh repaint and skips
//! ahead to it.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use quark_core::session::{Output, OutputStream};
use tokio::sync::broadcast;

/// Chunks buffered per session before a slow viewer loses some.
pub(crate) const CHANNEL: usize = 1024;

#[derive(Debug, Clone)]
pub(crate) struct Chunk {
    /// The bytes redraw the whole screen, starting from a terminal reset.
    pub repaint: bool,
    pub output: Output,
}

impl Chunk {
    pub fn bytes(data: Vec<u8>) -> Self {
        Self {
            repaint: false,
            output: Output::Bytes { data },
        }
    }

    pub fn repaint(data: Vec<u8>) -> Self {
        Self {
            repaint: true,
            output: Output::Bytes { data },
        }
    }

    pub fn exited(code: Option<i32>) -> Self {
        Self {
            repaint: false,
            output: Output::Exited { code },
        }
    }
}

/// Asks the backend to broadcast a repaint of one session.
pub(crate) type Resync = Arc<dyn Fn() + Send + Sync>;

pub(crate) struct Viewer {
    rx: broadcast::Receiver<Chunk>,
    /// Chunks that come out ahead of the broadcast, such as the attach
    /// repaint.
    first: VecDeque<Chunk>,
    synced: bool,
    ended: bool,
    resync: Resync,
}

impl Viewer {
    /// A viewer of `rx`. The `first` chunks come out before anything from
    /// `rx`; without a repaint among them the viewer waits for the next one.
    pub fn new(rx: broadcast::Receiver<Chunk>, first: Vec<Chunk>, resync: Resync) -> Self {
        Self {
            rx,
            first: first.into(),
            synced: false,
            ended: false,
            resync,
        }
    }

    fn accept(&mut self, c: Chunk) -> Option<Output> {
        if let Output::Exited { .. } = c.output {
            self.ended = true;
            return Some(c.output);
        }
        if c.repaint {
            self.synced = true;
        }
        self.synced.then_some(c.output)
    }
}

#[async_trait]
impl OutputStream for Viewer {
    async fn next(&mut self) -> Option<Output> {
        if self.ended {
            return None;
        }
        while let Some(c) = self.first.pop_front() {
            if let Some(o) = self.accept(c) {
                return Some(o);
            }
        }
        loop {
            match self.rx.recv().await {
                Ok(c) => {
                    if let Some(o) = self.accept(c) {
                        return Some(o);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    self.synced = false;
                    (self.resync)();
                }
                Err(broadcast::error::RecvError::Closed) => {
                    self.ended = true;
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[tokio::test]
    async fn waits_for_a_repaint_and_resyncs_after_lag() {
        let (tx, rx) = broadcast::channel(2);
        let asked = Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        let mut v = Viewer::new(
            rx,
            Vec::new(),
            Arc::new(move || {
                a.fetch_add(1, Ordering::SeqCst);
            }),
        );
        tx.send(Chunk::bytes(b"stale".to_vec())).unwrap();
        tx.send(Chunk::repaint(b"screen".to_vec())).unwrap();
        assert_eq!(
            v.next().await,
            Some(Output::Bytes {
                data: b"screen".to_vec()
            })
        );
        // Overflow the channel: the viewer asks for a repaint and skips to it.
        for _ in 0..3 {
            tx.send(Chunk::bytes(b"x".to_vec())).unwrap();
        }
        tx.send(Chunk::repaint(b"again".to_vec())).unwrap();
        assert_eq!(
            v.next().await,
            Some(Output::Bytes {
                data: b"again".to_vec()
            })
        );
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        tx.send(Chunk::exited(Some(0))).unwrap();
        assert_eq!(v.next().await, Some(Output::Exited { code: Some(0) }));
        assert_eq!(v.next().await, None);
    }
}
