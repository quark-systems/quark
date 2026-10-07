//! Child process for the crash tests in `tests/crash.rs`, which kill it at
//! random points. Not a tool for people.
//!
//! - `eventlog-crash append <db> <start>`: appends forever from payload
//!   `i = <start>`, alternating single
//!   events and three-event batches that also set the `writer` checkpoint,
//!   and prints `ack <seq> <id>` for each event once `append` returned.
//! - `eventlog-crash ingest <db> <home> <project>`: runs the firstmate
//!   bridge on `<home>` forever.

use std::io::Write;
use std::path::PathBuf;

use quark_core::{EventLog, HostId, NewEvent, ProjectId, TaskId};
use quark_eventlog::{FirstmateBridge, SqliteEventLog};

fn event(i: u64) -> NewEvent {
    NewEvent::new(
        HostId::from("crash"),
        ProjectId::from("p"),
        Some(TaskId::from("t")),
        "test.crash",
        serde_json::json!({ "i": i }),
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let log = SqliteEventLog::open(PathBuf::from(&args[1])).expect("open");
    match args[0].as_str() {
        "append" => {
            let mut out = std::io::stdout().lock();
            let mut i: u64 = args[2].parse().expect("start");
            loop {
                if i % 2 == 0 {
                    let e = event(i);
                    let id = e.id;
                    let seq = log.append(e).await.expect("append");
                    writeln!(out, "ack {} {id}", seq.0).unwrap();
                    i += 1;
                } else {
                    let batch: Vec<_> = (i..i + 3).map(event).collect();
                    let ids: Vec<_> = batch.iter().map(|e| e.id).collect();
                    let seqs = log
                        .append_batch(batch, Some(("writer".into(), (i + 2).to_string())))
                        .await
                        .expect("append batch");
                    for (seq, id) in seqs.iter().zip(ids) {
                        writeln!(out, "ack {} {id}", seq.0).unwrap();
                    }
                    i += 3;
                }
                out.flush().unwrap();
            }
        }
        "ingest" => {
            let bridge = FirstmateBridge::new(log, HostId::from("crash"));
            let home = PathBuf::from(&args[2]);
            let project = ProjectId::from(args[3].as_str());
            loop {
                bridge.ingest(&project, &home).await.expect("ingest");
                tokio::task::yield_now().await;
            }
        }
        other => panic!("unknown mode {other}"),
    }
}
