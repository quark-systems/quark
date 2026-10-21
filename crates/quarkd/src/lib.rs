//! `quarkd`: the Quark local control plane.
//!
//! Serves the `/v1` REST API and typed event stream on localhost, projects
//! engine state into SQLite, and reaches engine workspaces only through a
//! [`engine::EngineAdapter`].

pub mod accounts;
pub mod api;
pub mod beads;
pub mod chat;
pub mod classifier;
pub mod config;
pub mod coordinator_hands;
pub mod coordinator_http;
pub mod crew_dispatch;
pub mod dispatch;
pub mod dispatch_test;
pub mod engine;
pub mod failover;
pub mod forge;
pub mod gates;
pub mod harness;
pub mod host_sources;
pub mod hosts;
pub mod memory;
pub mod metrics;
pub mod native;
pub mod native_coordinator;
pub mod native_dispatch;
pub mod native_triggers;
pub mod native_worktrees;
pub mod overview;
pub mod pr_center;
pub mod project_repo;
pub mod projector;
pub mod provision;
pub mod recovery;
pub mod sessions;
pub mod settings;
pub mod spend;
pub mod store;
pub mod task_brief;
pub mod transcripts;
pub mod usage;
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

/// This machine until host identity lands with the host registry.
pub const LOCAL_HOST: &str = "local";

/// The host this daemon records events as.
pub fn host() -> quark_core::HostId {
    quark_core::HostId::from(LOCAL_HOST)
}

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
    let native_engine = engine == EngineKind::Native;
    // The engine opens its windows on the shared server, so it must be up
    // before the first engine write.
    if native_engine {
        if let Err(e) = sessions.ensure_server().await {
            tracing::warn!(error = %e, "terminal sessions unavailable");
        }
    }
    let events_path = config.events_path();
    let events = quark_eventlog::SqliteEventLog::open(&events_path)
        .with_context(|| format!("opening {}", events_path.display()))?;
    // The native engine is the native supervisor: it must run.
    let native = if native_engine {
        Some(
            native::NativeSupervision::start(&config, Arc::new(events.clone()))
                .await
                .context("starting the native engine's supervisor")?,
        )
    } else {
        None
    };
    // Workers of the native supervisor run in `quark-ptyd`; their terminals
    // come from there. Before any clone of the sessions.
    let sessions = match &native {
        Some(n) => sessions.with_pty(n.supervisor.sessions().clone(), store.clone()),
        None => sessions,
    };
    let layout = provision::Layout::new(&config.home).with_user_memory(config.user_memory.clone());
    // Native dispatch (slice 5) starts the native engine's tasks.
    let dispatch = match &native {
        Some(n) => {
            let parts = engine::native_dispatch_engine::DispatchParts::for_daemon(
                &config,
                Arc::new(events.clone()),
            );
            let d =
                engine::native_dispatch_engine::NativeDispatch::open(parts, n.supervisor.clone())
                    .await
                    .context("starting native dispatch")?;
            Some(d)
        }
        None => None,
    };
    let engine: Arc<dyn EngineAdapter> = match (&native, &dispatch) {
        (Some(n), Some(d)) => Arc::new(
            engine::native::NativeEngine::new(Arc::new(events.clone()), n.supervisor.clone())
                .with_dispatch(d.clone())
                .with_project_setup(Arc::new(engine::native_project::ProjectSetup::new(
                    sessions.clone(),
                    layout.clone(),
                    format!("http://{}", config.listen),
                ))),
        ),
        _ => config::build_engine(engine)?,
    };
    let forge: Arc<dyn forge::Forge> = Arc::new(forge::GhForge::default());
    let pr_center = pr_center::PrCenter::new(store.clone(), engine.clone(), forge.clone());
    let pr_task = tokio::spawn(pr_center.run(config.pr_refresh_interval));
    let harnesses = Arc::new(harness::HarnessRegistry::load(
        &config.home.join("harnesses"),
    ));
    let quota_axi = accounts::QuotaAxi {
        bin: config.quota_axi.clone(),
        local: native_dispatch::local_readings(&config),
    };
    let accounts = Arc::new(accounts::Accounts::new(
        store.clone(),
        harnesses.clone(),
        Arc::new(quota_axi),
        engine.account_envs(),
    ));
    let quota_task = tokio::spawn(accounts.clone().run(config.quota_refresh_interval));
    let dispatch_task = dispatch.map(|d| {
        d.set_accounts(Arc::new(engine::native_dispatch_engine::StoreAccounts(
            accounts.clone(),
        )));
        d.run(engine::native_dispatch_engine::TICK)
    });
    let failover = failover::Failover::new(
        store.clone(),
        engine.clone(),
        accounts.clone(),
        harnesses.clone(),
    );
    let projector = Projector::new(store.clone(), engine.clone())
        .with_sessions(sessions.clone())
        .with_command(layout.command_workspace())
        .with_user_config(layout.user_config())
        .with_failover(Arc::new(failover));
    let log: Arc<dyn quark_core::EventLog> = Arc::new(events.clone());
    // Host telemetry feeds the Hosts view and admission control.
    let host_view = host_sources::EngineView::new(
        store.clone(),
        engine.clone(),
        sessions.clone(),
        layout.command_workspace(),
    );
    // Admission control places the native engine's workers on registered,
    // sampled hosts.
    let (telemetry, pools_task) = if host_sources::enabled() || native_engine {
        let workloads = host_sources::EngineWorkloads(host_view.clone());
        let telemetry =
            match native_dispatch::HostTelemetry::start(&config, log.clone(), workloads).await {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "host telemetry not started");
                    None
                }
            };
        let pools = host_sources::PoolReporter::new(
            host_view,
            log.clone(),
            host(),
            native_worktrees::pool(log.clone(), host()).await,
        );
        (telemetry, Some(tokio::spawn(pools.run())))
    } else {
        (None, None)
    };
    let projector_task = tokio::spawn(projector.run(config.refresh_interval));
    // Coordinators come back after the tmux server is lost or a reboot.
    let recovery_task = native_engine.then(|| {
        let recovery = recovery::CoordinatorRecovery::new(
            store.clone(),
            engine.clone(),
            accounts.clone(),
            sessions.clone(),
            layout.command_workspace(),
            layout.home.clone(),
        )
        // A Project created under firstmate has no native coordinator record.
        .adopting(engine::native_project::WINDOW_PREFIX);
        tokio::spawn(recovery.run(recovery::INTERVAL))
    });
    let usage_task = tokio::spawn(
        usage::UsageIngest::new(
            store.clone(),
            events.clone(),
            host(),
            quark_transcript::SessionRoots::from_env(),
        )
        .run(config.refresh_interval),
    );
    // Channels, triggers and the away policy act on their own.
    let (triggers, triggers_task) = match native_triggers::Triggers::open(
        store.clone(),
        Arc::new(events.clone()),
        engine.clone(),
        &layout.home.join("channels"),
    )
    .await
    {
        Ok(t) => (
            Some(t.engine()),
            Some(tokio::spawn(t.run(config.refresh_interval))),
        ),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "slice 7 engine not started");
            (None, None)
        }
    };
    let chat_input: Arc<dyn chat::CoordinatorInput> = Arc::new(chat::SessionsInput::new(
        sessions.clone(),
        quark_transcript::SessionRoots::from_env(),
    ));
    // The native engine's coordinator wakes and acts.
    let (coordinator_routes, coordinator_task) = if native_engine {
        let wiring = native_coordinator::Wiring {
            brain: Arc::new(coordinator_hands::InputBrain::new(
                store.clone(),
                chat_input.clone(),
            )),
            hands: Arc::new(
                coordinator_hands::EngineHands::new(store.clone(), engine.clone())
                    .with_channels(triggers.clone()),
            ),
            base_url: format!("http://{}", config.listen),
        };
        match native_coordinator::NativeCoordinator::open(
            store.clone(),
            Arc::new(events.clone()),
            layout.clone(),
            quark_transcript::SessionRoots::from_env(),
            wiring,
        )
        .await
        {
            Ok(c) => {
                let c = c.with_sessions(sessions.clone());
                let routes = coordinator_http::router(
                    c.coordinator(),
                    c.tokens(),
                    store.clone(),
                    quark_transcript::SessionRoots::from_env(),
                );
                (
                    Some(routes),
                    Some(tokio::spawn(c.run(config.refresh_interval))),
                )
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "slice 6 coordinator not started");
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    let beads = Arc::new(
        beads::Beads::new(
            std::env::var_os("QUARK_BD").map(Into::into),
            format!("http://{}", config.listen),
        )
        .with_auto_setup(),
    );
    beads.watch_all(store.clone(), layout.home.clone());
    tokio::spawn({
        let (beads, store, home) = (beads.clone(), store.clone(), layout.home.clone());
        async move { beads.setup_missing(store, home).await }
    });

    let state = AppState {
        store,
        engine,
        harnesses,
        accounts,
        sessions: sessions.clone(),
        layout,
        chat: chat_input,
        forge,
        events,
        triggers,
        beads,
    };
    let decision_beads_task = api::spawn_decision_beads(state.clone());
    let mut app = api::router(state);
    if let Some(n) = &native {
        app = app.nest(native::WORKER_PREFIX, n.worker_router());
    }
    if let Some(routes) = coordinator_routes {
        app = app.nest(coordinator_http::PREFIX, routes);
    }
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    tracing::info!(addr = %listener.local_addr()?, db = %db_path.display(), "quarkd listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    projector_task.abort();
    if let Some(t) = recovery_task {
        t.abort();
    }
    usage_task.abort();
    decision_beads_task.abort();
    if let Some(t) = triggers_task {
        t.abort();
    }
    if let Some(t) = coordinator_task {
        t.abort();
    }
    pr_task.abort();
    quota_task.abort();
    if let Some(t) = dispatch_task {
        t.abort();
    }
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
