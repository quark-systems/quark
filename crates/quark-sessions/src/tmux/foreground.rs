//! What runs in the foreground of a tmux pane.
//!
//! A native session runs its agent as the pane's own process, so the agent
//! exiting ends the pane. Firstmate instead types its agent into a shell,
//! so the agent can exit while the pane, and its shell, stay alive. To tell
//! the two apart, [`TmuxBackend::foreground`](super::TmuxBackend::foreground)
//! reads the pane's foreground process group the way firstmate's tmux
//! backend does (`fm_backend_tmux_agent_state`): when every process in it is
//! a shell, the agent is gone.

/// Shell names, as firstmate's `fm_agent_process_classify_name` lists them.
const SHELLS: &[&str] = &[
    "zsh", "bash", "sh", "dash", "ash", "ksh", "mksh", "tcsh", "csh", "fish",
];

/// The pane's foreground process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Foreground {
    /// A program other than a shell runs: the agent, or something it
    /// started in the foreground.
    Program,
    /// Nothing but shells: the agent typed into the pane has exited.
    ShellOnly,
    /// The pane or its processes could not be read.
    Unknown,
}

/// Whether `comm` (a `ps` command name, possibly a path or a login shell's
/// `-zsh`) names a shell.
pub fn is_shell(comm: &str) -> bool {
    let base = comm.rsplit('/').next().unwrap_or(comm);
    SHELLS.contains(&base.trim_start_matches('-'))
}

/// Classifies `ps -o pid=,pgid=,tpgid=,stat=,comm=` rows for one terminal.
/// Only the foreground group (`pgid == tpgid`) counts, and zombies, which
/// have already exited, are left out.
pub fn classify(ps: &str) -> Foreground {
    let mut seen = false;
    for line in ps.lines() {
        let mut f = line.split_whitespace();
        let (Some(_pid), Some(pgid), Some(tpgid), Some(stat)) =
            (f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        let comm = f.collect::<Vec<_>>().join(" ");
        if comm.is_empty() || pgid != tpgid || stat.starts_with('Z') {
            continue;
        }
        if !is_shell(&comm) {
            return Foreground::Program;
        }
        seen = true;
    }
    if seen {
        Foreground::ShellOnly
    } else {
        Foreground::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_shells_like_firstmate() {
        assert!(is_shell("zsh"));
        assert!(is_shell("-zsh"));
        assert!(is_shell("/bin/bash"));
        assert!(!is_shell("claude"));
        assert!(!is_shell("node"));
        assert!(!is_shell("bashful"));
    }

    #[test]
    fn reads_only_the_foreground_group() {
        // The agent in the foreground, its shell parent in the background.
        let agent = "100 100 200 Ss -zsh\n200 200 200 S+ claude\n";
        assert_eq!(classify(agent), Foreground::Program);
        // The agent exited; its shell took the terminal back.
        let gone = "100 100 100 Ss+ -zsh\n";
        assert_eq!(classify(gone), Foreground::ShellOnly);
        // An exited agent tmux has not reaped yet is not running.
        let zombie = "100 100 100 Ss+ zsh\n200 100 100 Z+ claude\n";
        assert_eq!(classify(zombie), Foreground::ShellOnly);
        // A program the agent started in the foreground keeps it live.
        let tool = "100 100 300 Ss zsh\n300 300 300 S+ /usr/bin/python3\n";
        assert_eq!(classify(tool), Foreground::Program);
        assert_eq!(classify(""), Foreground::Unknown);
        assert_eq!(classify("garbage"), Foreground::Unknown);
    }
}
