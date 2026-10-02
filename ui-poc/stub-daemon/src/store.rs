//! In-memory event store with monotonic `seq`, bounded retention and
//! broadcast fan-out.
//!
//! Every event is serialised once, appended to the store and broadcast under
//! the same lock, so store order and broadcast order always agree. Clients
//! that lag the broadcast channel resynchronise from the store (see
//! `api::ws_session`), so a slow client never blocks publishers.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde::Serialize;
use serde_json::json;
use tokio::sync::broadcast;

use crate::model::now_ts;

/// Retained `worker.output` bytes per worker (raw terminal bytes).
const WORKER_RETENTION_BYTES: usize = 2 * 1024 * 1024;
/// Retained count of all other events.
const GENERAL_RETENTION_EVENTS: usize = 20_000;
/// Live channel depth before a receiver is considered lagged.
const BROADCAST_CAPACITY: usize = 4096;

/// A serialised event ready to be written to a websocket.
#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub seq: u64,
    pub json: Arc<str>,
}

#[derive(Default)]
struct Inner {
    last_seq: u64,
    events: BTreeMap<u64, Arc<str>>,
    /// Seqs of non-worker-output events, oldest first.
    general: VecDeque<u64>,
    /// Per worker: (seq, byte length) of retained output events, plus total.
    worker: HashMap<String, (VecDeque<(u64, usize)>, usize)>,
}

pub struct Hub {
    inner: Mutex<Inner>,
    tx: broadcast::Sender<StoredEvent>,
}

impl Hub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: Mutex::new(Inner::default()),
            tx,
        }
    }

    /// Publish a domain event (anything except `worker.output`).
    pub fn publish(&self, project_id: &str, kind: &str, payload: &impl Serialize) -> u64 {
        let payload = serde_json::to_value(payload).expect("payload serialises");
        self.append(project_id, kind, payload, None)
    }

    /// Publish raw terminal bytes from a worker pane.
    pub fn publish_worker_output(&self, project_id: &str, worker_id: &str, data: &[u8]) -> u64 {
        let payload = json!({
            "worker_id": worker_id,
            "data_b64": base64::engine::general_purpose::STANDARD.encode(data),
        });
        self.append(
            project_id,
            "worker.output",
            payload,
            Some((worker_id, data.len())),
        )
    }

    fn append(
        &self,
        project_id: &str,
        kind: &str,
        payload: serde_json::Value,
        worker: Option<(&str, usize)>,
    ) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        inner.last_seq += 1;
        let seq = inner.last_seq;
        let json: Arc<str> = json!({
            "seq": seq,
            "project_id": project_id,
            "type": kind,
            "ts": now_ts(),
            "payload": payload,
        })
        .to_string()
        .into();
        inner.events.insert(seq, json.clone());

        // Retention: evict the oldest events of the same class.
        let mut evict = Vec::new();
        match worker {
            Some((worker_id, len)) => {
                let (queue, total) = inner.worker.entry(worker_id.to_string()).or_default();
                queue.push_back((seq, len));
                *total += len;
                while *total > WORKER_RETENTION_BYTES && queue.len() > 1 {
                    let (old, old_len) = queue.pop_front().unwrap();
                    *total -= old_len;
                    evict.push(old);
                }
            }
            None => {
                inner.general.push_back(seq);
                while inner.general.len() > GENERAL_RETENTION_EVENTS {
                    evict.push(inner.general.pop_front().unwrap());
                }
            }
        }
        for old in evict {
            inner.events.remove(&old);
        }

        // Sent under the lock so channel order == seq order. Errors only
        // mean nobody is subscribed right now.
        let _ = self.tx.send(StoredEvent { seq, json });
        seq
    }

    /// Every retained event with `seq > cursor`, in order.
    pub fn since(&self, cursor: u64) -> Vec<StoredEvent> {
        let inner = self.inner.lock().unwrap();
        inner
            .events
            .range(cursor + 1..)
            .map(|(&seq, json)| StoredEvent {
                seq,
                json: json.clone(),
            })
            .collect()
    }

    /// Seq of the most recently published event (0 if none).
    pub fn head(&self) -> u64 {
        self.inner.lock().unwrap().last_seq
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StoredEvent> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_is_monotonic_and_replay_respects_cursor() {
        let hub = Hub::new();
        let a = hub.publish("p", "task.created", &json!({}));
        let b = hub.publish("p", "task.created", &json!({}));
        assert!(b > a);
        let replay = hub.since(a);
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].seq, b);
    }

    #[test]
    fn worker_output_retention_is_bounded() {
        let hub = Hub::new();
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..100 {
            hub.publish_worker_output("p", "w1", &chunk);
        }
        let retained = hub.since(0).len() * chunk.len();
        assert!(retained <= WORKER_RETENTION_BYTES);
        assert!(retained >= WORKER_RETENTION_BYTES - chunk.len());
    }
}
