//! `quarkd`: the Quark local control plane.
//!
//! Serves the `/v1` REST API and typed event stream on localhost, projects
//! engine state into SQLite, and reaches engine workspaces only through a
//! [`engine::EngineAdapter`].

pub mod api;
pub mod chat;
pub mod config;
pub mod dispatch;
pub mod engine;
pub mod forge;
pub mod gates;
pub mod harness;
pub mod pr_center;
pub mod project_repo;
pub mod projector;
pub mod provision;
pub mod sessions;
pub mod store;
pub mod transcripts;
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
    let layout = provision::Layout::new(&config.home);
    let projector = Projector::new(store.clone(), engine.clone())
        .with_sessions(sessions.clone())
        .with_command(layout.command_workspace());
    let projector_task = tokio::spawn(projector.run(config.refresh_interval));
    let forge: Arc<dyn forge::Forge> = Arc::new(forge::GhForge::default());
    let pr_center = pr_center::PrCenter::new(store.clone(), engine.clone(), forge.clone());
    let pr_task = tokio::spawn(pr_center.run(config.pr_refresh_interval));

    let app = api::router(AppState {
        store,
        engine,
        harnesses: Arc::new(harness::HarnessRegistry::builtin()),
        sessions: sessions.clone(),
        layout,
        chat: Arc::new(chat::SessionsInput::new(
            sessions.clone(),
            quark_transcript::SessionRoots::from_env(),
        )),
        forge,
    });
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    tracing::info!(addr = %listener.local_addr()?, db = %db_path.display(), "quarkd listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    projector_task.abort();
    pr_task.abort();
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
