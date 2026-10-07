//! The daemon mirrors each Project's firstmate home into `events.db`.

use std::sync::Arc;

use quark_core::{EventLog, Seq};
use quark_eventlog::{FirstmateBridge, SqliteEventLog};
use quark_systems::CreateProject;
use quarkd::event_ingest::{self, EventIngest};
use quarkd::store::Store;

#[tokio::test]
async fn ingests_every_project_workspace() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("state")).unwrap();
    std::fs::write(home.path().join("state/t1.status"), "working: started\n").unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let project = store
        .create_project(CreateProject {
            name: "Quark".into(),
            goal: None,
            workspace_path: Some(home.path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .unwrap();
    store
        .create_project(CreateProject {
            name: "No workspace".into(),
            ..Default::default()
        })
        .unwrap();

    let db = tempfile::tempdir().unwrap();
    let log = SqliteEventLog::open(db.path().join("events.db")).unwrap();
    let ingest = EventIngest::new(
        store,
        FirstmateBridge::new(log.clone(), event_ingest::host()),
    );
    ingest.ingest_all().await.unwrap();
    ingest.ingest_all().await.unwrap();

    let events = log.read(Seq::ZERO, 10).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].project.as_str(), project.id);
    assert_eq!(events[0].kind.as_str(), "firstmate.status");
    assert_eq!(events[0].payload["verb"], "working");
}
