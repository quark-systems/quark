//! `quark-ptyd --socket <path>`: the PTY supervisor as its own process.
//!
//! Runs one [`quark_sessions::pty::PtySupervisor`] on a Unix socket so its
//! sessions outlive the daemon that started it. Refuses to start when
//! another `quark-ptyd` already answers on the socket. `quarkd ptyd` runs
//! the same thing.

use std::path::PathBuf;

use quark_sessions::pty::run_daemon;

const USAGE: &str = "usage: quark-ptyd --socket <path>";

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let socket = match (args.next().as_deref(), args.next(), args.next()) {
        (Some("--socket"), Some(path), None) => PathBuf::from(path),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run_daemon(&socket).await {
        eprintln!("quark-ptyd: {e}");
        std::process::exit(1);
    }
}
