//! Opening a pseudo-terminal and starting a program on it.

use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};

use quark_core::session::{SessionSpec, TermSize};
use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
use rustix::termios::{tcsetwinsize, Winsize};

/// Environment every program gets unless the spec sets it.
const DEFAULT_ENV: [(&str, &str); 2] = [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")];

/// A started program and the controlling side of its terminal.
pub struct Spawned {
    pub master: File,
    pub child: Child,
}

/// Opens a pseudo-terminal sized `spec.size` and starts `spec.argv` on it in
/// a new session, with the terminal as its controlling terminal. An empty
/// argv runs `$SHELL`, or `/bin/sh`.
pub fn spawn(spec: &SessionSpec) -> std::io::Result<Spawned> {
    let master: OwnedFd = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    set_size(master.as_fd(), spec.size)?;
    let name = ptsname(&master, Vec::new())?;
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(rustix::fs::OFlags::NOCTTY.bits() as i32)
        .open(std::ffi::OsStr::new(
            std::str::from_utf8(name.as_bytes()).map_err(std::io::Error::other)?,
        ))?;

    let (program, args) = match spec.argv.split_first() {
        Some((p, a)) => (p.clone(), a.to_vec()),
        None => (
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            Vec::new(),
        ),
    };
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    for (k, v) in DEFAULT_ENV {
        if !spec.env.contains_key(k) {
            cmd.env(k, v);
        }
    }
    cmd.envs(&spec.env);
    // SAFETY: setsid and the TIOCSCTTY ioctl are single system calls, safe
    // between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            rustix::process::setsid()?;
            rustix::process::ioctl_tiocsctty(BorrowedFd::borrow_raw(0))?;
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    // `cmd` holds the last copies of the terminal's program side; drop them
    // so reads on the master end once the program exits.
    drop(cmd);
    Ok(Spawned {
        master: File::from(master),
        child,
    })
}

pub fn set_size(master: BorrowedFd<'_>, size: TermSize) -> std::io::Result<()> {
    tcsetwinsize(
        master,
        Winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )?;
    Ok(())
}
