//! Stub daemon for the Quark UI proof-of-concept.
//!
//! Serves the API in `ui-poc/CONTRACT.md` on 127.0.0.1:7420 with realistic
//! fake data, background activity, a streaming fake coordinator chat, and
//! four live terminal panes backed by a private tmux server.

mod api;
mod app;
mod data;
mod model;
mod store;
mod tmux;

use std::sync::Arc;

use crate::app::App;
use crate::store::Hub;

fn parse_port() -> u16 {
    let mut port = 7420;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" | "-p" => {
                port = args.next().and_then(|p| p.parse().ok()).unwrap_or_else(|| {
                    eprintln!("--port needs a number");
                    std::process::exit(2);
                });
            }
            "--help" | "-h" => {
                println!("quark-ui-stub-daemon [--port PORT]   (default 7420)");
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    port
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}

#[tokio::main]
async fn main() {
    let port = parse_port();
    let hub = Arc::new(Hub::new());
    tmux::set_socket(port);

    let bindings: Vec<(&str, &str, &str, &str)> = data::WORKER_TASKS
        .iter()
        .enumerate()
        .map(|(i, (task_id, title))| {
            let project = if task_id.starts_with("t-2") {
                "firstmate"
            } else {
                "quark"
            };
            (["w-1", "w-2", "w-3", "w-4"][i], *task_id, project, *title)
        })
        .collect();
    let tmux = match tmux::Tmux::start(hub.clone(), &bindings) {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("warning: tmux workers unavailable: {e}");
            None
        }
    };

    let app = Arc::new(App::new(hub, tmux));
    app.spawn_background();

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot listen on {addr}: {e}");
            if let Some(t) = &app.tmux {
                t.shutdown();
            }
            std::process::exit(1);
        }
    };
    eprintln!("quark-ui-stub-daemon listening on http://{addr}");

    // On Ctrl-C / SIGTERM: kill the tmux server and exit immediately rather
    // than waiting for long-lived websocket sessions to drain.
    let on_signal = app.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        eprintln!("shutting down; killing tmux server");
        if let Some(t) = &on_signal.tmux {
            t.shutdown();
        }
        std::process::exit(0);
    });

    if let Err(e) = axum::serve(listener, api::router(app.clone())).await {
        eprintln!("server error: {e}");
    }
    if let Some(t) = &app.tmux {
        t.shutdown();
    }
}
