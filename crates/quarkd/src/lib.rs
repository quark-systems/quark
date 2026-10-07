//! `quarkd`: the Quark local control plane.
//!
//! Serves the `/v1` REST API and typed event stream on localhost, projects
//! engine state into SQLite, and reaches engine workspaces only through a
//! [`engine::EngineAdapter`].

pub mod accounts;
pub mod api;
pub mod chat;
pub mod classifier;
pub mod config;
pub mod crew_dispatch;
pub mod dispatch;
pub mod dispatch_test;
pub mod engine;
pub mod event_ingest;
pub mod failover;
pub mod forge;
pub mod gates;
pub mod harness;
pub mod host_sources;
pub mod hosts;
pub mod memory;
pub mod metrics;
pub mod native;
pub mod native_dispatch;
pub mod native_triggers;
pub mod overview;
pub mod pr_center;
pub mod project_repo;
pub mod projector;
pub mod provision;
pub mod sessions;
pub mod settings;
pub mod store;
pub mod transcripts;
pub mod verify_shadow;
pub mod worktree;

use std::sync::Arc;

use crate::engine::EngineAdapter;
use anyhow::Context;
use tokio::net::TcpListener;

use crate::api::AppState;
use crate::config::{Config, EngineKind};
use crate::projector::Projector;
use crate::sessions::Sessions;
use crate::store::Store;

/// Current UTC time as RFC 3339.
pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC 3339 formatting never fails for UTC")
}

/// Opens the store, builds the engine adapter, starts the projector and
/// serves the API until shutdown.
pub async fn serve(config: Config, engine: EngineKind) -> anyhow::Result<()> {
    if !config.listen.ip().is_loopback() {
        anyhow::bail!(
            "refusing to listen on non-loopback address {}; Local mode serves localhost only",
            config.listen
        );
    }
    std::fs::create_dir_all(config.home.join("logs"))
        .with_context(|| format!("creating {}", config.home.display()))?;
    let db_path = config.db_path();
    let store =
        Arc::new(Store::open(&db_path).with_context(|| format!("opening {}", db_path.display()))?);
    let sessions = Sessions::detect(config.tmux.as_deref(), config.run_dir(), store.clone());
    // The engine opens its windows on the shared server, so it must be up
    // before the first engine write.
    if engine == EngineKind::Firstmate {
        if let Err(e) = sessions.ensure_server().await {
            tracing::warn!(error = %e, "terminal sessions unavailable");
        }
    }
    let tmux = sessions.tmux_env().ok();
    let engine: Arc<dyn EngineAdapter> =
        config::build_engine(engine, &config, store.clone(), tmux)?;
    let layout = provision::Layout::new(&config.home).with_user_memory(config.user_memory.clone());
    let forge: Arc<dyn forge::Forge> = Arc::new(forge::GhForge::default());
    let pr_center = pr_center::PrCenter::new(store.clone(), engine.clone(), forge.clone());
    let pr_task = tokio::spawn(pr_center.run(config.pr_refresh_interval));
    let harnesses = Arc::new(harness::HarnessRegistry::load(
        &config.home.join("harnesses"),
    ));
    let quota_axi = accounts::QuotaAxi {
        bin: config.quota_axi.clone(),
    };
    let accounts = Arc::new(accounts::Accounts::new(
        store.clone(),
        harnesses.clone(),
        Arc::new(quota_axi),
        engine.account_envs(),
    ));
    let quota_task = tokio::spawn(accounts.clone().run(config.quota_refresh_interval));
    let failover = failover::Failover::new(
        store.clone(),
        engine.clone(),
        accounts.clone(),
        harnesses.clone(),
    );
    let events_path = config.events_path();
    let events = quark_eventlog::SqliteEventLog::open(&events_path)
        .with_context(|| format!("opening {}", events_path.display()))?;
    let mut projector = Projector::new(store.clone(), engine.clone())
        .with_sessions(sessions.clone())
        .with_command(layout.command_workspace())
        .with_user_config(layout.user_config())
        .with_failover(Arc::new(failover));
    let log: Arc<dyn quark_core::EventLog> = Arc::new(events.clone());
    if native_dispatch::enabled() {
        projector = projector.with_dispatch_shadow(Arc::new(native_dispatch::DispatchShadow::new(
            &config,
            log.clone(),
        )));
    }
    // Host telemetry feeds the Hosts view and admission control.
    let host_view = host_sources::EngineView::new(
        store.clone(),
        engine.clone(),
        sessions.clone(),
        layout.command_workspace(),
    );
    let (telemetry, pools_task) = if host_sources::enabled() || native_dispatch::enabled() {
        let workloads = host_sources::EngineWorkloads(host_view.clone());
        let telemetry =
            match native_dispatch::HostTelemetry::start(&config, log.clone(), workloads).await {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "host telemetry not started");
                    None
                }
            };
        let pools = host_sources::PoolReporter::new(host_view, log.clone(), event_ingest::host());
        (telemetry, Some(tokio::spawn(pools.run())))
    } else {
        (None, None)
    };
    let projector_task = tokio::spawn(projector.run(config.refresh_interval));
    let native = if native::enabled() {
        match native::NativeSupervision::start(&config, Arc::new(events.clone())).await {
            Ok(n) => Some(n),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "native supervisor not started");
                None
            }
        }
    } else {
        None
    };
    let ingest = event_ingest::EventIngest::new(
        store.clone(),
        quark_eventlog::FirstmateBridge::new(events.clone(), event_ingest::host()),
    );
    let ingest_task = tokio::spawn(ingest.run(config.refresh_interval));
    let (triggers, triggers_task) = if native_triggers::enabled() {
        match native_triggers::ShadowTriggers::open(store.clone(), Arc::new(events.clone())).await {
            Ok(t) => (
                Some(t.engine()),
                Some(tokio::spawn(t.run(config.refresh_interval))),
            ),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "slice 7 shadow not started");
                (None, None)
            }
        }
    } else {
        (None, None)
    };
    let slices = engine::shadow::slices_from_env()?;
    let verify_task = (slices.mode(quark_core::Slice::Verification)
        == quark_core::SliceMode::Shadow)
        .then(|| {
            tracing::info!(
                "slice 2 (verification) in shadow: comparing firstmate's guard decisions"
            );
            let shadow = quark_verify::ShadowVerifier::new(
                Arc::new(verify_shadow::LogCheckpoints(events.clone())),
                Some(Arc::new(quark_verify::GhForge::default())),
                event_ingest::host(),
            );
            tokio::spawn(
                verify_shadow::VerifyShadow::new(store.clone(), shadow)
                    .run(config.refresh_interval),
            )
        });

    let mut app = api::router(AppState {
        store,
        engine,
        harnesses,
        accounts,
        sessions: sessions.clone(),
        layout,
        chat: Arc::new(chat::SessionsInput::new(
            sessions.clone(),
            quark_transcript::SessionRoots::from_env(),
        )),
        forge,
        events,
        triggers,
    });
    if let Some(n) = &native {
        app = app.nest(native::WORKER_PREFIX, n.worker_router());
    }
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    tracing::info!(addr = %listener.local_addr()?, db = %db_path.display(), "quarkd listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    projector_task.abort();
    ingest_task.abort();
    if let Some(t) = triggers_task {
        t.abort();
    }
    if let Some(t) = verify_task {
        t.abort();
    }
    pr_task.abort();
    quota_task.abort();
    if let Some(n) = native {
        n.stop();
    }
    if let Some(t) = telemetry {
        t.stop();
    }
    if let Some(t) = pools_task {
        t.abort();
    }
    // The tmux server keeps running; the next start reattaches.
    sessions.detach_all();
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
    tracing::info!("shutting down");
}
