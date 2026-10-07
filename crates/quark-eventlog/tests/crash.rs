//! Kill-at-random-point crash tests for the event log and the firstmate
//! bridge. Each round starts the `eventlog-crash` child, lets it write for a
//! random moment, SIGKILLs it and checks the file.
//!
//! Set `QUARK_CRASH_SEED` to replay a failing run; the seed is in every
//! assertion message. `QUARK_CRASH_ROUNDS` raises the round count.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use quark_core::{EventLog, NewEvent, Seq};
use quark_eventlog::firstmate::{kinds, StatusPayload};
use quark_eventlog::{FirstmateBridge, SqliteEventLog};

const CHILD: &str = env!("CARGO_BIN_EXE_eventlog-crash");

/// xorshift64*: enough randomness to pick kill points, and replayable.
struct Rng(u64);

impl Rng {
    fn from_env() -> Self {
        let seed = std::env::var("QUARK_CRASH_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos() as u64
            });
        Rng(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn rounds() -> u64 {
    std::env::var("QUARK_CRASH_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20)
}

fn spawn(args: &[&str]) -> Child {
    Command::new(CHILD)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start crash child")
}

/// SIGKILL `child` after `ms` and return everything it printed.
fn kill_after(mut child: Child, ms: u64) -> Vec<String> {
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        BufReader::new(stdout)
            .lines()
            .map_while(|l| l.ok())
            .collect::<Vec<_>>()
    });
    std::thread::sleep(Duration::from_millis(ms));
    child.kill().unwrap();
    child.wait().unwrap();
    reader.join().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn appends_survive_kill_at_random_points() {
    let mut rng = Rng::from_env();
    let seed = rng.0;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("events.db");
    let db_arg = db.to_str().unwrap();
    let mut acked: HashMap<u64, String> = HashMap::new();
    let mut start = 0u64;

    for round in 0..rounds() {
        let ctx = format!("seed {seed} round {round}");
        let from = start.to_string();
        let printed = kill_after(spawn(&["append", db_arg, &from]), 5 + rng.below(80));
        for line in printed {
            let mut parts = line.split_whitespace();
            assert_eq!(parts.next(), Some("ack"), "{ctx}: {line}");
            let seq: u64 = parts.next().unwrap().parse().unwrap();
            let id = parts.next().unwrap().to_string();
            assert!(
                acked.insert(seq, id).is_none(),
                "{ctx}: seq {seq} acked twice"
            );
        }

        let log = SqliteEventLog::open(&db).unwrap();
        let head = log.verify().await.unwrap_or_else(|e| panic!("{ctx}: {e}"));
        let events = log.read(Seq::ZERO, usize::MAX).await.unwrap();
        assert_eq!(events.len() as u64, head.0, "{ctx}");
        // Every acknowledged event is there, at the seq it was given.
        for (seq, id) in &acked {
            let e = &events[(*seq - 1) as usize];
            assert_eq!(&e.id.to_string(), id, "{ctx}: seq {seq}");
        }
        // Batches are all or nothing, and the checkpoint moved with them.
        if let Some(mark) = log.checkpoint("writer").await.unwrap() {
            let mark: u64 = mark.parse().unwrap();
            let has = |i: u64| events.iter().any(|e| e.payload["i"] == i);
            assert!(
                has(mark) && has(mark - 1) && has(mark - 2),
                "{ctx}: checkpoint {mark}"
            );
        }
        let ii: Vec<u64> = events
            .iter()
            .map(|e| e.payload["i"].as_u64().unwrap())
            .collect();
        assert!(ii.windows(2).all(|w| w[0] < w[1]), "{ctx}: payload order");
        start = ii.last().map_or(0, |i| i + 1);
        // Re-appending a stored event is a no-op returning its seq.
        if let Some(e) = events.last() {
            let again = NewEvent {
                id: e.id,
                ts: e.ts,
                host: e.host.clone(),
                project: e.project.clone(),
                task: e.task.clone(),
                kind: e.kind.clone(),
                payload: e.payload.clone(),
            };
            assert_eq!(log.append(again).await.unwrap(), e.seq, "{ctx}");
        }
    }
    assert!(
        !acked.is_empty(),
        "seed {seed}: the child never acknowledged an append"
    );
}

fn append(path: &Path, text: &str) {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn ingest_lands_each_line_once_across_kills() {
    let mut rng = Rng::from_env();
    let seed = rng.0;
    let home = tempfile::tempdir().unwrap();
    let state = home.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let db_dir = tempfile::tempdir().unwrap();
    let db = db_dir.path().join("events.db");
    let tasks = ["t1", "t2", "t3"];
    let verbs = ["working", "needs-decision", "resolved", "paused", "done"];
    let mut n = 0;

    for round in 0..rounds() {
        let child = spawn(&[
            "ingest",
            db.to_str().unwrap(),
            home.path().to_str().unwrap(),
            "p",
        ]);
        // Write while the bridge reads, sometimes a line in two pieces.
        let writes = 1 + rng.below(40);
        for _ in 0..writes {
            let task = tasks[rng.below(3) as usize];
            let line = format!(
                "{} [key=k{}]: line {n}\n",
                verbs[rng.below(5) as usize],
                rng.below(4)
            );
            n += 1;
            let path = state.join(format!("{task}.status"));
            if rng.below(4) == 0 {
                let (a, b) = line.split_at(line.len() / 2);
                append(&path, a);
                std::thread::sleep(Duration::from_micros(rng.below(500)));
                append(&path, b);
            } else {
                append(&path, &line);
            }
        }
        kill_after(child, rng.below(30));
        let log = SqliteEventLog::open(&db).unwrap();
        log.verify()
            .await
            .unwrap_or_else(|e| panic!("seed {seed} round {round}: {e}"));
    }

    // One last pass in-process picks up whatever the last kill cut off.
    let log = SqliteEventLog::open(&db).unwrap();
    let bridge = FirstmateBridge::new(log.clone(), "h".into());
    bridge.ingest(&"p".into(), home.path()).await.unwrap();
    let events = log.read(Seq::ZERO, usize::MAX).await.unwrap();
    for task in tasks {
        let want: Vec<String> = std::fs::read_to_string(state.join(format!("{task}.status")))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        let got: Vec<String> = events
            .iter()
            .filter(|e| e.kind.as_str() == kinds::STATUS)
            .filter(|e| e.task.as_ref().map(|t| t.as_str()) == Some(task))
            .map(|e| e.decode::<StatusPayload>().unwrap().raw)
            .collect();
        assert_eq!(got, want, "seed {seed}: {task}");
    }
}
