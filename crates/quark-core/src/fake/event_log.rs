use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::Notify;

use crate::{Event, EventId, EventLog, NewEvent, Result, Seq, Subscription};

#[derive(Default)]
struct Inner {
    events: Vec<Event>,
    by_id: HashMap<EventId, Seq>,
}

/// An [`EventLog`] in memory, with the same ordering and idempotency rules
/// as the real one. Not durable.
#[derive(Default, Clone)]
pub struct MemoryEventLog {
    inner: Arc<Mutex<Inner>>,
    appended: Arc<Notify>,
}

impl MemoryEventLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every event, oldest first.
    pub fn events(&self) -> Vec<Event> {
        self.inner.lock().unwrap().events.clone()
    }

    fn after(&self, after: Seq, limit: usize) -> Vec<Event> {
        let inner = self.inner.lock().unwrap();
        let start = (after.0 as usize).min(inner.events.len());
        inner.events[start..].iter().take(limit).cloned().collect()
    }
}

#[async_trait]
impl EventLog for MemoryEventLog {
    async fn append(&self, event: NewEvent) -> Result<Seq> {
        let seq = {
            let mut inner = self.inner.lock().unwrap();
            if let Some(seq) = inner.by_id.get(&event.id) {
                return Ok(*seq);
            }
            let seq = Seq(inner.events.len() as u64 + 1);
            inner.by_id.insert(event.id, seq);
            inner.events.push(event.with_seq(seq));
            seq
        };
        self.appended.notify_waiters();
        Ok(seq)
    }

    async fn read(&self, after: Seq, limit: usize) -> Result<Vec<Event>> {
        Ok(self.after(after, limit))
    }

    async fn head(&self) -> Result<Seq> {
        Ok(Seq(self.inner.lock().unwrap().events.len() as u64))
    }

    async fn subscribe(&self, after: Seq) -> Result<Box<dyn Subscription>> {
        Ok(Box::new(MemorySubscription {
            log: self.clone(),
            at: after,
        }))
    }
}

struct MemorySubscription {
    log: MemoryEventLog,
    at: Seq,
}

#[async_trait]
impl Subscription for MemorySubscription {
    async fn next(&mut self) -> Option<Result<Event>> {
        loop {
            // Register before checking, so an append in between is not missed.
            let notified = self.log.appended.notified();
            if let Some(e) = self.log.after(self.at, 1).pop() {
                self.at = e.seq;
                return Some(Ok(e));
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HostId, ProjectId};

    fn ev(kind: &str) -> NewEvent {
        NewEvent::new(
            HostId::from("h"),
            ProjectId::from("p"),
            None,
            kind,
            serde_json::json!({}),
        )
    }

    #[tokio::test]
    async fn append_is_ordered_and_idempotent() {
        let log = MemoryEventLog::new();
        let a = ev("a.x");
        assert_eq!(log.append(a.clone()).await.unwrap(), Seq(1));
        assert_eq!(log.append(ev("b.x")).await.unwrap(), Seq(2));
        assert_eq!(log.append(a).await.unwrap(), Seq(1));
        assert_eq!(log.head().await.unwrap(), Seq(2));
        let after_one = log.read(Seq(1), 10).await.unwrap();
        assert_eq!(after_one.len(), 1);
        assert_eq!(after_one[0].kind.as_str(), "b.x");
    }

    #[tokio::test]
    async fn subscription_catches_up_then_follows() {
        let log = MemoryEventLog::new();
        log.append(ev("a.x")).await.unwrap();
        let mut sub = log.subscribe(Seq::ZERO).await.unwrap();
        assert_eq!(sub.next().await.unwrap().unwrap().seq, Seq(1));
        let writer = log.clone();
        let task = tokio::spawn(async move { sub.next().await.unwrap().unwrap().seq });
        tokio::task::yield_now().await;
        writer.append(ev("b.x")).await.unwrap();
        assert_eq!(task.await.unwrap(), Seq(2));
    }
}
