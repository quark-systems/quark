//! The output stream both backends hand to viewers.
//!
//! A backend broadcasts [`Chunk`]s per session. A viewer only starts
//! forwarding at a repaint (a chunk that redraws the whole screen from a
//! reset), so it never sees output it has no screen for. When a viewer falls
//! behind and loses chunks it skips what the channel still holds, then asks
//! the backend for a fresh repaint and waits for it.

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

    /// After a lag: reads past what the channel still holds, up to a repaint
    /// or the session's exit. Without either, asks the backend for a repaint.
    ///
    /// The backend is asked only once nothing is left to read. A lagging
    /// viewer stands at the oldest chunk of a full channel, so a repaint
    /// broadcast then would push out the very chunk it reads next: it would
    /// lag again on its own request, ask again, and never reach a repaint.
    fn catch_up(&mut self) -> Option<Output> {
        self.synced = false;
        loop {
            match self.rx.try_recv() {
                Ok(c) => {
                    if let Some(o) = self.accept(c) {
                        return Some(o);
                    }
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                // A closed channel is reported by the next `recv`.
                Err(_) => break,
            }
        }
        (self.resync)();
        None
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
                    if let Some(o) = self.catch_up() {
                        return Some(o);
                    }
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
    use std::time::Duration;

    use super::*;

    fn bytes(data: &str) -> Option<Output> {
        Some(Output::Bytes {
            data: data.as_bytes().to_vec(),
        })
    }

    /// A viewer whose backend answers a resync the way the PTY supervisor
    /// does: with a repaint broadcast on the viewer's own channel, at once.
    fn viewer(tx: &broadcast::Sender<Chunk>) -> (Viewer, Arc<AtomicUsize>) {
        let asked = Arc::new(AtomicUsize::new(0));
        let (a, repaint) = (asked.clone(), tx.clone());
        let resync = Arc::new(move || {
            let n = a.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = repaint.send(Chunk::repaint(format!("repaint {n}").into_bytes()));
        });
        (Viewer::new(tx.subscribe(), Vec::new(), resync), asked)
    }

    async fn next(v: &mut Viewer) -> Option<Output> {
        tokio::time::timeout(Duration::from_secs(30), v.next())
            .await
            .expect("the viewer never caught up")
    }

    #[tokio::test]
    async fn waits_for_a_repaint_and_resyncs_after_lag() {
        let (tx, _) = broadcast::channel(2);
        let (mut v, asked) = viewer(&tx);
        tx.send(Chunk::bytes(b"stale".to_vec())).unwrap();
        tx.send(Chunk::repaint(b"screen".to_vec())).unwrap();
        assert_eq!(next(&mut v).await, bytes("screen"));
        assert_eq!(asked.load(Ordering::SeqCst), 0);

        // Overflow the channel: the viewer asks for one repaint and skips
        // to it. The repaint lands in a channel that was full, and the
        // viewer must not lose its place to it and ask again.
        for _ in 0..3 {
            tx.send(Chunk::bytes(b"x".to_vec())).unwrap();
        }
        assert_eq!(next(&mut v).await, bytes("repaint 1"));
        assert_eq!(asked.load(Ordering::SeqCst), 1);

        // Output after the repaint follows it.
        tx.send(Chunk::bytes(b"tail".to_vec())).unwrap();
        assert_eq!(next(&mut v).await, bytes("tail"));
        tx.send(Chunk::exited(Some(0))).unwrap();
        assert_eq!(next(&mut v).await, Some(Output::Exited { code: Some(0) }));
        assert_eq!(next(&mut v).await, None);
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_lagging_viewer_takes_a_repaint_the_channel_already_holds() {
        let (tx, _) = broadcast::channel(2);
        let (mut v, asked) = viewer(&tx);
        tx.send(Chunk::repaint(b"screen".to_vec())).unwrap();
        assert_eq!(next(&mut v).await, bytes("screen"));
        for _ in 0..3 {
            tx.send(Chunk::bytes(b"x".to_vec())).unwrap();
        }
        tx.send(Chunk::repaint(b"again".to_vec())).unwrap();
        assert_eq!(next(&mut v).await, bytes("again"));
        assert_eq!(asked.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_lagging_viewer_still_sees_the_exit() {
        let (tx, _) = broadcast::channel(2);
        let (mut v, asked) = viewer(&tx);
        tx.send(Chunk::repaint(b"screen".to_vec())).unwrap();
        assert_eq!(next(&mut v).await, bytes("screen"));
        for _ in 0..3 {
            tx.send(Chunk::bytes(b"x".to_vec())).unwrap();
        }
        tx.send(Chunk::exited(Some(7))).unwrap();
        assert_eq!(next(&mut v).await, Some(Output::Exited { code: Some(7) }));
        assert_eq!(next(&mut v).await, None);
        assert_eq!(asked.load(Ordering::SeqCst), 0);
    }
}
