//! Test parts: a fake coordinator agent that speaks the inbox and parent
//! channel contract, a git origin to clone, a stand-in `ssh`, and
//! sub-coordinators wired to them over real tmux and the SQLite log.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use quark_core::{EventLog, HostId, ProjectId};
use quark_harness::ManifestRegistry;
use quark_runtime::{Options, RuntimeSpec, SshTarget};
use quark_subcoordinator::{
    Charter, Config, Placement, Profile, ProjectSource, Registration, RuntimeHosts, SubCoordinators,
};

/// Reads its inbox whenever a line arrives, acts on a few words, moves
/// each message to `handled`, and reports on its parent channel.
const AGENT: &str = r#"#!/bin/sh
echo "coordinator ready"
echo "working: started $QUARK_GENERATION" >> "$QUARK_PARENT_CHANNEL"
while IFS= read -r line; do
  for f in "$QUARK_INBOX"/*; do
    [ -f "$f" ] || continue
    name=$(basename "$f")
    body=$(cat "$f")
    mv "$f" "$QUARK_INBOX/handled/$name"
    printf '%s\n' "$name" >> "$QUARK_HOME/took"
    case "$body" in
      *crash*) exit 3 ;;
      *ask*) echo "needs-decision [key=k1]: which way?" >> "$QUARK_PARENT_CHANNEL" ;;
      *"Answer to [k1]"*) echo "working: going left" >> "$QUARK_PARENT_CHANNEL" ;;
    esac
  done
done
"#;

/// Runs the remote command line with this machine's `sh`; exits 255 (a
/// failed connection) while `$dir/down` exists.
const FAKE_SSH: &str = r#"#!/bin/sh
dir=$(dirname "$0")
[ -e "$dir/down" ] && { echo "ssh: connect to host: Connection refused" >&2; exit 255; }
while [ $# -gt 2 ]; do shift; done
exec sh -c "$2"
"#;

pub fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub struct Env {
    pub dir: tempfile::TempDir,
    pub origin: PathBuf,
    pub manifests: Arc<ManifestRegistry>,
    pub socket: String,
    pub ssh: PathBuf,
}

impl Env {
    pub fn new(tag: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "-b", "main"]);
        git(&origin, &["config", "user.email", "t@example.com"]);
        git(&origin, &["config", "user.name", "t"]);
        std::fs::write(origin.join("README"), "hi\n").unwrap();
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-qm", "init"]);

        let agent = dir.path().join("agent.sh");
        std::fs::write(&agent, AGENT).unwrap();
        let ssh = dir.path().join("bin/ssh");
        std::fs::create_dir_all(ssh.parent().unwrap()).unwrap();
        std::fs::write(&ssh, FAKE_SSH).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();

        let coordinator = quark_harness::parse(&format!(
            r#"
schema = 1
id = "fake"
name = "Fake coordinator"
roles = ["coordinator", "worker"]
efforts = ["low", "high"]
[detect]
bins = ["sh"]
[launch]
argv = ["sh", "{agent}"]
prompt_via = "paste"
[turn_signals]
busy = "none"
turn_end = "none"
idle_patterns = ["coordinator ready"]
[keys]
interrupt = ["C-c"]
exit = "/exit"
"#,
            agent = agent.display()
        ))
        .unwrap();
        let worker_only = quark_harness::parse(
            r#"
schema = 1
id = "workeronly"
name = "Worker only"
roles = ["worker"]
[detect]
bins = ["sh"]
[launch]
argv = ["sh"]
prompt_via = "paste"
[turn_signals]
busy = "none"
turn_end = "none"
[keys]
interrupt = ["C-c"]
exit = "/exit"
"#,
        )
        .unwrap();
        let missing = quark_harness::parse(
            r#"
schema = 1
id = "missing"
name = "Not installed"
roles = ["coordinator"]
[detect]
bins = ["quark-test-no-such-agent"]
install_hint = "install the test agent"
[launch]
argv = ["quark-test-no-such-agent"]
prompt_via = "paste"
[turn_signals]
busy = "none"
turn_end = "none"
[keys]
interrupt = ["C-c"]
exit = "/exit"
"#,
        )
        .unwrap();
        Self {
            origin,
            manifests: Arc::new(ManifestRegistry::with([coordinator, worker_only, missing])),
            socket: format!("quark-sub-{tag}-{}", std::process::id()),
            ssh,
            dir,
        }
    }

    pub fn log(&self) -> Arc<dyn EventLog> {
        Arc::new(quark_eventlog::SqliteEventLog::open(self.dir.path().join("events.db")).unwrap())
    }

    pub fn config(&self) -> Config {
        let mut c = Config::new(HostId::from("parent"));
        c.ready_timeout = Duration::from_secs(5);
        c.ready_quiet = Duration::from_millis(300);
        c
    }

    pub fn hosts(&self) -> Arc<RuntimeHosts> {
        Arc::new(
            RuntimeHosts::new(Options {
                ssh_program: self.ssh.clone(),
                connect_timeout: Duration::from_secs(2),
            })
            .with_socket(&self.socket),
        )
    }

    /// A new engine on the same log, as after a daemon restart.
    pub async fn open(&self) -> SubCoordinators {
        SubCoordinators::open(
            self.log(),
            self.manifests.clone(),
            self.hosts(),
            self.config(),
        )
        .await
        .unwrap()
    }

    pub fn home(&self, id: &str) -> PathBuf {
        self.dir.path().join("homes").join(id)
    }

    pub fn registration(&self, id: &str, runtime: RuntimeSpec) -> Registration {
        let host = match &runtime {
            RuntimeSpec::Local => HostId::from("parent"),
            RuntimeSpec::Ssh { target } => HostId::from(target.destination.as_str()),
        };
        Registration {
            id: ProjectId::from(id),
            charter: Charter {
                scope: format!("{id} work"),
                instructions: "Keep main green.".into(),
            },
            placement: Placement {
                host,
                runtime,
                home: self.home(id),
            },
            profile: Profile {
                harness: "fake".into(),
                model: None,
                effort: Some("low".into()),
            },
            projects: vec![ProjectSource {
                name: "app".into(),
                origin: self.origin.display().to_string(),
            }],
        }
    }

    pub fn ssh_spec(&self) -> RuntimeSpec {
        RuntimeSpec::Ssh {
            target: SshTarget::new("build-box"),
        }
    }

    /// Make the stand-in SSH host unreachable, or reachable again.
    pub fn set_down(&self, down: bool) {
        let marker = self.ssh.parent().unwrap().join("down");
        if down {
            std::fs::write(marker, "").unwrap();
        } else {
            let _ = std::fs::remove_file(marker);
        }
    }

    pub fn tmux(&self, args: &[&str]) -> std::process::Output {
        Command::new("tmux")
            .args(["-L", &self.socket])
            .args(args)
            .output()
            .unwrap()
    }

    pub fn took(&self, id: &str) -> Vec<String> {
        std::fs::read_to_string(self.home(id).join("took"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}

/// Run `f` until it is true, ticking in between, or panic after 10s.
pub async fn eventually(what: &str, mut f: impl AsyncFnMut() -> bool) {
    for _ in 0..100 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}
