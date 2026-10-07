//! `quark-ptyd --socket <path>`: the PTY supervisor as its own process.
//!
//! Runs one [`PtySupervisor`] on a Unix socket so its sessions outlive the
//! daemon that started it. Refuses to start when another `quark-ptyd`
//! already answers on the socket.

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use quark_sessions::pty::{serve, PtySupervisor};
use tokio::net::{UnixListener, UnixStream};

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
    if let Err(e) = run(socket).await {
        eprintln!("quark-ptyd: {e}");
        std::process::exit(1);
    }
}

async fn run(socket: PathBuf) -> std::io::Result<()> {
    if UnixStream::connect(&socket).await.is_ok() {
        return Err(std::io::Error::other(format!(
            "another supervisor is listening on {}",
            socket.display()
        )));
    }
    if let Some(dir) = socket
        .parent()
        .filter(|d| !d.as_os_str().is_empty() && !d.exists())
    {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    // Nothing answered, so a socket file left here belongs to a dead one.
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    serve(listener, PtySupervisor::new()).await
}
