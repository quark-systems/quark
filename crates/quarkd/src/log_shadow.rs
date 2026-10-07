//! Shadows that judge what is already in the event log.
//!
//! Some native slices are compared with firstmate not on an adapter call
//! but on events: a status line a worker wrote, a coordinator turn. A
//! [`Judge`] looks at each event in log order and says where the native
//! side would have read or decided it differently; [`LogShadow`] feeds it
//! every event and appends what it finds as `shadow.divergence` events.
//!
//! How far it has judged is a checkpoint written in the same transaction
//! as the divergences, so each event is judged once however the daemon
//! dies. On start the judge still sees the whole log from the beginning,
//! so a judge that keeps state rebuilds it; only events past the
//! checkpoint can record a divergence.

use std::time::Duration;

use quark_core::event::kinds;
use quark_core::slice::Divergence;
use quark_core::{Event, EventLog, HostId, NewEvent, ProjectId, Seq, Slice};
use quark_eventlog::SqliteEventLog;

const READ_BATCH: usize = 1000;

/// One disagreement a judge found.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// The Project it is recorded under; `None` for the judged event's.
    pub project: Option<ProjectId>,
    pub operation: String,
    pub bash: serde_json::Value,
    pub native: serde_json::Value,
}

/// Compares the native side of one slice with firstmate, event by event.
pub trait Judge: Send {
    /// The slice its divergences count toward.
    fn slice(&self) -> Slice;
    /// The checkpoint that remembers how far it judged.
    fn checkpoint(&self) -> &'static str;
    /// Look at one event. `shadow.divergence` events are never passed.
    fn judge(&mut self, event: &Event) -> Vec<Found>;
}

/// What one [`LogShadow::pass`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pass {
    /// Events judged for the first time.
    pub judged: usize,
    pub diverged: usize,
}

/// Runs a [`Judge`] over the log.
pub struct LogShadow<J> {
    log: SqliteEventLog,
    host: HostId,
    judge: J,
    /// Read up to here.
    at: Seq,
    /// Divergences recorded up to here; `None` until the first pass reads
    /// the checkpoint.
    through: Option<Seq>,
}

impl<J: Judge> LogShadow<J> {
    pub fn new(log: SqliteEventLog, host: HostId, judge: J) -> Self {
        Self {
            log,
            host,
            judge,
            at: Seq::ZERO,
            through: None,
        }
    }

    /// Judge everything appended since the last pass.
    pub async fn pass(&mut self) -> quark_core::Result<Pass> {
        let name = self.judge.checkpoint();
        let through = match self.through {
            Some(t) => t,
            None => {
                let saved = self.log.checkpoint(name).await?;
                let t = Seq(saved.and_then(|v| v.parse().ok()).unwrap_or(0));
                self.through = Some(t);
                t
            }
        };
        let mut pass = Pass::default();
        let mut found = Vec::new();
        loop {
            let events = self.log.read(self.at, READ_BATCH).await?;
            let Some(last) = events.last().map(|e| e.seq) else {
                break;
            };
            for e in &events {
                if e.kind.as_str() == kinds::SHADOW_DIVERGENCE {
                    continue;
                }
                let divergences = self.judge.judge(e);
                if e.seq <= through {
                    continue;
                }
                pass.judged += 1;
                for f in divergences {
                    let d = Divergence {
                        slice: self.judge.slice(),
                        operation: f.operation,
                        bash: f.bash,
                        native: f.native,
                    };
                    let (project, task) = match f.project {
                        Some(p) if p != e.project => (p, None),
                        _ => (e.project.clone(), e.task.clone()),
                    };
                    found.push(NewEvent::typed(
                        self.host.clone(),
                        project,
                        task,
                        kinds::SHADOW_DIVERGENCE,
                        &d,
                    )?);
                }
            }
            self.at = last;
        }
        if self.at > through {
            pass.diverged = found.len();
            self.log
                .append_batch(found, Some((name.to_string(), self.at.0.to_string())))
                .await?;
            self.through = Some(self.at);
        }
        Ok(pass)
    }

    pub async fn run(mut self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match self.pass().await {
                Ok(p) if p.diverged > 0 => tracing::info!(
                    slice = %self.judge.slice(),
                    judged = p.judged,
                    diverged = p.diverged,
                    "shadow disagreed with firstmate"
                ),
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(slice = %self.judge.slice(), error = %e, "shadow pass failed")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use quark_core::{ProjectId, TaskId};

    use super::*;

    /// Counts events, and disagrees with every `x` event.
    struct Counter {
        seen: usize,
    }

    impl Judge for Counter {
        fn slice(&self) -> Slice {
            Slice::WorkerProtocol
        }
        fn checkpoint(&self) -> &'static str {
            "shadow/test"
        }
        fn judge(&mut self, e: &Event) -> Vec<Found> {
            self.seen += 1;
            if e.kind.as_str() != "x" {
                return Vec::new();
            }
            vec![Found {
                project: None,
                operation: "x".into(),
                bash: serde_json::json!(self.seen),
                native: serde_json::Value::Null,
            }]
        }
    }

    async fn append(log: &SqliteEventLog, kind: &str) {
        let e = NewEvent::new(
            HostId::from("h"),
            ProjectId::from("p"),
            Some(TaskId::from("t")),
            kind,
            serde_json::json!({}),
        );
        log.append(e).await.unwrap();
    }

    async fn divergences(log: &SqliteEventLog) -> Vec<Divergence> {
        log.read(Seq::ZERO, 100)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.kind.as_str() == kinds::SHADOW_DIVERGENCE)
            .map(|e| e.decode().unwrap())
            .collect()
    }

    #[tokio::test]
    async fn judges_each_event_once_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let log = SqliteEventLog::open(dir.path().join("events.db")).unwrap();
        append(&log, "y").await;
        append(&log, "x").await;
        let mut s = LogShadow::new(log.clone(), "h".into(), Counter { seen: 0 });
        assert_eq!(
            s.pass().await.unwrap(),
            Pass {
                judged: 2,
                diverged: 1
            }
        );
        assert_eq!(s.pass().await.unwrap(), Pass::default());

        // A new daemon: the judge replays everything, records only new.
        append(&log, "x").await;
        let mut again = LogShadow::new(log.clone(), "h".into(), Counter { seen: 0 });
        assert_eq!(
            again.pass().await.unwrap(),
            Pass {
                judged: 1,
                diverged: 1
            }
        );
        let d = divergences(&log).await;
        assert_eq!(d.len(), 2);
        assert_eq!(d[1].slice, Slice::WorkerProtocol);
        // The judge saw y, x and x: its own divergence was not passed.
        assert_eq!(d[1].bash, serde_json::json!(3));
    }
}
