//! Read models: views rebuilt from the event log by replay.

use async_trait::async_trait;

use crate::{Event, EventLog, Result, Seq};

/// A projection of the log. quarkd's projector is one; the dashboard's
/// metrics are another. Applying the same event twice must be harmless, so
/// recovery is "replay from [`ReadModel::applied_through`]".
#[async_trait]
pub trait ReadModel: Send + Sync {
    /// The last sequence number this model has applied.
    async fn applied_through(&self) -> Result<Seq>;

    /// Apply one event. Events arrive in `seq` order.
    async fn apply(&self, event: &Event) -> Result<()>;

    /// Forget everything, so the next replay starts from `Seq::ZERO`.
    async fn reset(&self) -> Result<()>;
}

/// Bring `model` up to the head of `log` and return the new position.
pub async fn replay(log: &dyn EventLog, model: &dyn ReadModel, batch: usize) -> Result<Seq> {
    let batch = batch.max(1);
    let mut at = model.applied_through().await?;
    loop {
        let events = log.read(at, batch).await?;
        let Some(last) = events.last().map(|e| e.seq) else {
            return Ok(at);
        };
        for e in &events {
            model.apply(e).await?;
        }
        at = last;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::fake::MemoryEventLog;
    use crate::{HostId, NewEvent, ProjectId};

    #[derive(Default)]
    struct Count(Mutex<(Seq, u32)>);

    #[async_trait]
    impl ReadModel for Count {
        async fn applied_through(&self) -> Result<Seq> {
            Ok(self.0.lock().unwrap().0)
        }
        async fn apply(&self, e: &Event) -> Result<()> {
            let mut s = self.0.lock().unwrap();
            *s = (e.seq, s.1 + 1);
            Ok(())
        }
        async fn reset(&self) -> Result<()> {
            *self.0.lock().unwrap() = (Seq::ZERO, 0);
            Ok(())
        }
    }

    #[tokio::test]
    async fn replay_resumes_where_it_stopped() {
        let log = MemoryEventLog::new();
        for _ in 0..5 {
            let e = NewEvent::new(
                HostId::from("h"),
                ProjectId::from("p"),
                None,
                "t.x",
                serde_json::Value::Null,
            );
            log.append(e).await.unwrap();
        }
        let m = Count::default();
        assert_eq!(replay(&log, &m, 2).await.unwrap(), Seq(5));
        assert_eq!(m.0.lock().unwrap().1, 5);
        // Nothing new: nothing applied twice.
        replay(&log, &m, 2).await.unwrap();
        assert_eq!(m.0.lock().unwrap().1, 5);
        m.reset().await.unwrap();
        replay(&log, &m, 100).await.unwrap();
        assert_eq!(m.0.lock().unwrap().1, 5);
    }
}
