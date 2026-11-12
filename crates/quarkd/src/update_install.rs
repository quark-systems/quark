//! Installing an update: `quarkd update-install`, the quark-updater.
//!
//! quarkd can't replace the bundle it runs from and then judge the result,
//! so `POST /v1/update/install` backs up the home, copies its own binary to
//! `<home>/updates/quark-updater`, writes the install's [`Intent`] and starts
//! that copy in its own process group (so launchd restarting quarkd doesn't
//! take it along). The updater then:
//!
//! 1. unpacks the verified bundle beside the installed app (same volume);
//! 2. quits Quark and QuarkBar, noting which were open;
//! 3. moves the installed app aside (kept for Roll back) and the new one in;
//! 4. restarts quarkd (`launchctl kickstart -k`), and waits up to two minutes
//!    for `/v1/health` to report the new version, twice;
//! 5. reopens QuarkBar, and Quark if it was open.
//!
//! If the new quarkd never comes up healthy, it puts the old app back, asks
//! for the pre-update backup to be restored when the old build can't read
//! what the new one wrote ([`crate::backup::request_restore`]), restarts the
//! old quarkd and reopens the apps. Workers keep running throughout: their
//! ptyd outlives quarkd, and the new quarkd speaks the old ptyd's protocol.
//! `quarkd update-install --rollback` runs the same steps back to the app
//! kept aside (QuarkBar's Roll back).
//!
//! Every step is recorded in `<home>/updates/intent.json`, which
//! `GET /v1/update` reports as `last_install`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};

/// How long a new quarkd has to report healthy.
pub const HEALTH_GATE: Duration = Duration::from_secs(120);
/// The healthy reports a new quarkd must give, this far apart.
const HEALTH_CHECKS: u32 = 2;
const HEALTH_APART: Duration = Duration::from_secs(5);
/// The installed app moved aside, kept beside it for Roll back.
pub const PREVIOUS: &str = ".Quark-previous.app";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// The updater is about to start.
    Pending,
    /// The new app is unpacked and the old one still in place.
    Staged,
    /// The new app is in place; quarkd is being checked.
    Swapped,
    /// The new version is running.
    Done,
    /// The new version failed its health check, or was rolled back by hand,
    /// and the old one is back.
    RolledBack,
    /// Neither version came up; see `error`.
    Failed,
}

/// One install or rollback, written before the updater starts and updated by
/// each of its steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Intent {
    /// The version being replaced.
    pub from: String,
    /// The version being installed (for a rollback, the one going back to).
    pub to: String,
    /// A rollback to the app kept aside, rather than an update.
    #[serde(default)]
    pub rollback: bool,
    /// The installed Quark.app.
    #[schema(value_type = String)]
    pub app: PathBuf,
    /// The verified update bundle (`.app.tar.gz`); empty for a rollback.
    #[serde(default)]
    #[schema(value_type = String)]
    pub bundle: PathBuf,
    /// The pre-update backup of the home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub backup: Option<PathBuf>,
    /// Going back needs [`Self::backup`] restored: the version going back to
    /// can't read the data the other one writes.
    #[serde(default)]
    pub restore_on_rollback: bool,
    /// quarkd's health endpoint.
    pub health_url: String,
    pub phase: Phase,
    /// RFC 3339.
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Intent {
    pub fn path(home: &Path) -> PathBuf {
        home.join("updates").join("intent.json")
    }

    pub fn load(home: &Path) -> Option<Intent> {
        serde_json::from_slice(&std::fs::read(Self::path(home)).ok()?).ok()
    }

    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        let path = Self::path(home);
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// The version in place when this phase is reached: what an undone install
    /// went back to, otherwise where it was headed.
    pub fn installed(&self) -> &str {
        if self.phase == Phase::RolledBack && !self.rollback {
            &self.from
        } else {
            &self.to
        }
    }

    /// Whether an updater is (or should be) working on it.
    pub fn in_progress(&self) -> bool {
        matches!(self.phase, Phase::Pending | Phase::Staged | Phase::Swapped)
    }
}

pub fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// The Quark.app holding `exe`, a quarkd at
/// `Quark.app/Contents/Library/LoginItems/QuarkBar.app/Contents/MacOS/quarkd`:
/// the outermost `.app` on its path.
pub fn installed_app(exe: &Path) -> Option<PathBuf> {
    let mut found = None;
    for a in exe.ancestors() {
        if a.extension().is_some_and(|e| e == "app") {
            found = Some(a.to_path_buf());
        }
    }
    found.filter(|app| {
        app.join("Contents/Library/LoginItems/QuarkBar.app")
            .is_dir()
    })
}

/// What the updater does to the machine around the bundle swap. [`Mac`] in
/// a release; tests fake it.
pub trait Host {
    /// Quits Quark and QuarkBar; returns whether Quark was open.
    fn quit_apps(&self, app: &Path) -> anyhow::Result<bool>;
    /// Restarts quarkd from the installed bundle.
    fn restart_quarkd(&self) -> anyhow::Result<()>;
    /// Opens QuarkBar, and Quark too when `desktop`.
    fn open_apps(&self, app: &Path, desktop: bool);
    /// The version quarkd's health endpoint reports, if it answers.
    fn health_version(&self, url: &str) -> Option<String>;
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d)
    }
}

/// Runs the install or rollback `<home>/updates/intent.json` describes.
pub fn run(home: &Path, host: &dyn Host, gate: Duration) -> anyhow::Result<Intent> {
    let mut intent = Intent::load(home)
        .ok_or_else(|| anyhow!("no update in {}", Intent::path(home).display()))?;
    anyhow::ensure!(
        intent.phase == Phase::Pending,
        "the update to {} is already {:?}",
        intent.to,
        intent.phase
    );
    let result = if intent.rollback {
        roll_back(home, host, gate, &mut intent, None)
    } else {
        install(home, host, gate, &mut intent)
    };
    if let Err(e) = &result {
        intent.phase = Phase::Failed;
        intent.error = Some(format!("{e:#}"));
    }
    intent.finished_at = Some(now());
    intent.save(home)?;
    result.map(|()| intent)
}

fn step(home: &Path, intent: &mut Intent, phase: Phase) -> anyhow::Result<()> {
    intent.phase = phase;
    tracing::info!(?phase, version = %intent.installed(), "update");
    intent.save(home)
}

fn install(
    home: &Path,
    host: &dyn Host,
    gate: Duration,
    intent: &mut Intent,
) -> anyhow::Result<()> {
    let app = intent.app.clone();
    let parent = app
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent", app.display()))?;
    let staging = parent.join(format!(".Quark-update-{}", intent.to));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let staged = unpack(&intent.bundle, &staging)?;
    step(home, intent, Phase::Staged)?;

    let desktop = host.quit_apps(&app)?;
    let previous = parent.join(PREVIOUS);
    let _ = std::fs::remove_dir_all(&previous);
    std::fs::rename(&app, &previous).with_context(|| format!("moving {} aside", app.display()))?;
    if let Err(e) = std::fs::rename(&staged, &app) {
        std::fs::rename(&previous, &app)?;
        host.open_apps(&app, desktop);
        bail!("moving the new app into place: {e}");
    }
    let _ = std::fs::remove_dir_all(&staging);
    step(home, intent, Phase::Swapped)?;

    host.restart_quarkd()?;
    match wait_healthy(host, &intent.health_url, &intent.to, gate) {
        Ok(()) => {
            host.open_apps(&app, desktop);
            step(home, intent, Phase::Done)
        }
        Err(why) => {
            tracing::warn!(%why, "update failed its health check; rolling back");
            let why = format!("Quark {} didn't start: {why}", intent.to);
            roll_back(home, host, gate, intent, Some((why, desktop)))
        }
    }
}

/// Puts the app kept aside back. `failed` carries why an install is being
/// undone and whether Quark was open; `None` for Roll back by hand.
fn roll_back(
    home: &Path,
    host: &dyn Host,
    gate: Duration,
    intent: &mut Intent,
    failed: Option<(String, bool)>,
) -> anyhow::Result<()> {
    let app = intent.app.clone();
    let parent = app
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent", app.display()))?;
    let previous = parent.join(PREVIOUS);
    anyhow::ensure!(
        previous.is_dir(),
        "no earlier Quark kept at {}",
        previous.display()
    );
    let undoing = failed.is_some();
    let (error, desktop) = match failed {
        Some((why, desktop)) => (Some(why), desktop),
        None => (None, host.quit_apps(&app)?),
    };
    let back_to = if undoing {
        intent.from.clone()
    } else {
        intent.to.clone()
    };
    let undone = parent.join(".Quark-undone.app");
    let _ = std::fs::remove_dir_all(&undone);
    std::fs::rename(&app, &undone)?;
    std::fs::rename(&previous, &app)?;
    // Keep what was undone, so a rollback can itself be rolled back.
    std::fs::rename(&undone, &previous)?;
    if intent.restore_on_rollback {
        let backup = intent
            .backup
            .as_deref()
            .ok_or_else(|| anyhow!("this rollback needs a backup and there is none"))?;
        crate::backup::request_restore(home, backup)?;
    }
    host.restart_quarkd()?;
    let healthy = wait_healthy(host, &intent.health_url, &back_to, gate);
    host.open_apps(&app, desktop);
    healthy.map_err(|why| {
        anyhow!(
            "{}Quark {back_to} didn't start either: {why}",
            error
                .as_deref()
                .map(|e| format!("{e}. "))
                .unwrap_or_default()
        )
    })?;
    intent.error = error;
    step(home, intent, Phase::RolledBack)
}

/// Unpacks a `.app.tar.gz` into `into`; returns the app it held.
fn unpack(bundle: &Path, into: &Path) -> anyhow::Result<PathBuf> {
    let out = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(bundle)
        .arg("-C")
        .arg(into)
        .output()
        .context("running tar")?;
    if !out.status.success() {
        bail!(
            "unpacking {}: {}",
            bundle.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let apps: Vec<PathBuf> = std::fs::read_dir(into)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "app") && p.is_dir())
        .collect();
    match apps.as_slice() {
        [app] if app.join("Contents/Info.plist").is_file() => Ok(app.clone()),
        _ => bail!("{} doesn't hold one app bundle", bundle.display()),
    }
}

fn wait_healthy(host: &dyn Host, url: &str, version: &str, gate: Duration) -> Result<(), String> {
    let start = Instant::now();
    let mut seen = 0;
    let mut answered = false;
    let mut last: Option<String>;
    loop {
        match host.health_version(url) {
            Some(v) if v == version => {
                answered = true;
                seen += 1;
                if seen >= HEALTH_CHECKS {
                    return Ok(());
                }
                host.sleep(HEALTH_APART);
                continue;
            }
            other => {
                seen = 0;
                last = other;
            }
        }
        if start.elapsed() >= gate {
            return Err(match last {
                Some(v) => format!("quarkd {v} answered instead of {version}"),
                None if answered => format!(
                    "quarkd {version} didn't stay up for {HEALTH_CHECKS} checks in a row within {}s",
                    gate.as_secs()
                ),
                None => format!("quarkd didn't answer within {}s", gate.as_secs()),
            });
        }
        host.sleep(Duration::from_secs(2));
    }
}

/// macOS: AppleScript to quit the apps, launchctl to restart quarkd, open to
/// reopen them, curl for health.
pub struct Mac;

const DESKTOP_ID: &str = "systems.quark.desktop";
const BAR_ID: &str = "systems.quark.bar";
const QUARKD_LABEL: &str = "systems.quark.quarkd";

fn osascript(script: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl Mac {
    fn running(id: &str) -> bool {
        osascript(&format!("application id \"{id}\" is running")).as_deref() == Some("true")
    }
}

impl Host for Mac {
    fn quit_apps(&self, app: &Path) -> anyhow::Result<bool> {
        let desktop = Mac::running(DESKTOP_ID);
        for id in [DESKTOP_ID, BAR_ID] {
            if Mac::running(id) {
                let _ = osascript(&format!("tell application id \"{id}\" to quit"));
            }
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        while (Mac::running(DESKTOP_ID) || Mac::running(BAR_ID)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(500));
        }
        // Whatever still runs from the bundle (quarkd excepted: launchd restarts it).
        for exe in [
            "Contents/MacOS/",
            "Contents/Library/LoginItems/QuarkBar.app/Contents/MacOS/QuarkBar",
        ] {
            let _ = std::process::Command::new("/usr/bin/pkill")
                .args(["-f", &app.join(exe).to_string_lossy()])
                .status();
        }
        Ok(desktop)
    }

    fn restart_quarkd(&self) -> anyhow::Result<()> {
        let uid = std::process::Command::new("/usr/bin/id")
            .arg("-u")
            .output()?;
        let uid = String::from_utf8_lossy(&uid.stdout).trim().to_string();
        let status = std::process::Command::new("/bin/launchctl")
            .args(["kickstart", "-k", &format!("gui/{uid}/{QUARKD_LABEL}")])
            .status()?;
        anyhow::ensure!(status.success(), "launchctl kickstart exited {status}");
        Ok(())
    }

    fn open_apps(&self, app: &Path, desktop: bool) {
        let bar = app.join("Contents/Library/LoginItems/QuarkBar.app");
        let _ = std::process::Command::new("/usr/bin/open")
            .arg("-g")
            .arg(&bar)
            .status();
        if desktop {
            let _ = std::process::Command::new("/usr/bin/open")
                .arg(app)
                .status();
        }
    }

    fn health_version(&self, url: &str) -> Option<String> {
        let out = std::process::Command::new("curl")
            .args(["-fsS", "-m", "5", url])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        v.get("version")?.as_str().map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// Pretends to be launchd and the apps: "quarkd" reports the version in
    /// the installed app's `version` file after each restart, unless that
    /// version is set to never come up.
    struct Fake {
        app: PathBuf,
        broken: Option<String>,
        log: RefCell<Vec<String>>,
        running: RefCell<Option<String>>,
        answers: RefCell<VecDeque<Option<String>>>,
    }

    impl Fake {
        fn new(app: &Path) -> Self {
            Fake {
                app: app.into(),
                broken: None,
                log: Default::default(),
                running: Default::default(),
                answers: Default::default(),
            }
        }
    }

    impl Host for Fake {
        fn quit_apps(&self, _: &Path) -> anyhow::Result<bool> {
            self.log.borrow_mut().push("quit".into());
            Ok(true)
        }
        fn restart_quarkd(&self) -> anyhow::Result<()> {
            let v = std::fs::read_to_string(self.app.join("Contents/version")).unwrap();
            self.log.borrow_mut().push(format!("restart {v}"));
            *self.running.borrow_mut() = (self.broken.as_ref() != Some(&v)).then_some(v);
            Ok(())
        }
        fn open_apps(&self, _: &Path, desktop: bool) {
            self.log
                .borrow_mut()
                .push(format!("open desktop={desktop}"));
        }
        fn health_version(&self, _: &str) -> Option<String> {
            if let Some(a) = self.answers.borrow_mut().pop_front() {
                return a;
            }
            self.running.borrow().clone()
        }
        fn sleep(&self, _: Duration) {}
    }

    fn make_app(dir: &Path, version: &str) {
        std::fs::create_dir_all(dir.join("Contents/Library/LoginItems/QuarkBar.app")).unwrap();
        std::fs::write(dir.join("Contents/Info.plist"), "plist").unwrap();
        std::fs::write(dir.join("Contents/version"), version).unwrap();
    }

    struct Setup {
        _root: tempfile::TempDir,
        home: PathBuf,
        app: PathBuf,
    }

    fn setup(rollback_restore: bool) -> Setup {
        let root = tempfile::tempdir().unwrap();
        let apps = root.path().join("Applications");
        let app = apps.join("Quark.app");
        make_app(&app, "0.1.0");
        let build = root.path().join("build");
        make_app(&build.join("Quark.app"), "0.2.0");
        let bundle = root.path().join("Quark.app.tar.gz");
        let ok = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&bundle)
            .arg("-C")
            .arg(&build)
            .arg("Quark.app")
            .status()
            .unwrap();
        assert!(ok.success());
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let backup = crate::backup::snapshot(&home, "0.2.0").unwrap();
        Intent {
            from: "0.1.0".into(),
            to: "0.2.0".into(),
            rollback: false,
            app: app.clone(),
            bundle,
            backup: Some(backup),
            restore_on_rollback: rollback_restore,
            health_url: "http://127.0.0.1:1/v1/health".into(),
            phase: Phase::Pending,
            started_at: now(),
            finished_at: None,
            error: None,
        }
        .save(&home)
        .unwrap();
        Setup {
            _root: root,
            home,
            app,
        }
    }

    fn version(app: &Path) -> String {
        std::fs::read_to_string(app.join("Contents/version")).unwrap()
    }

    #[test]
    fn installs_and_keeps_the_old_app_for_roll_back() {
        let s = setup(false);
        let host = Fake::new(&s.app);
        let done = run(&s.home, &host, Duration::from_secs(1)).unwrap();
        assert_eq!(done.phase, Phase::Done);
        assert_eq!(version(&s.app), "0.2.0");
        let previous = s.app.parent().unwrap().join(PREVIOUS);
        assert_eq!(version(&previous), "0.1.0");
        assert_eq!(
            *host.log.borrow(),
            ["quit", "restart 0.2.0", "open desktop=true"]
        );
        assert!(!s.app.parent().unwrap().join(".Quark-update-0.2.0").exists());
        assert_eq!(Intent::load(&s.home).unwrap().phase, Phase::Done);
        // Running it again does nothing.
        assert!(run(&s.home, &host, Duration::from_secs(1)).is_err());

        // Roll back by hand, then forward again.
        let mut back = Intent::load(&s.home).unwrap();
        (back.from, back.to, back.rollback, back.phase) =
            ("0.2.0".into(), "0.1.0".into(), true, Phase::Pending);
        back.save(&s.home).unwrap();
        let back = run(&s.home, &host, Duration::from_secs(1)).unwrap();
        assert_eq!(back.phase, Phase::RolledBack);
        assert_eq!(back.error, None);
        assert_eq!(version(&s.app), "0.1.0");
        assert_eq!(version(&previous), "0.2.0");
    }

    #[test]
    fn an_update_that_doesnt_come_up_is_rolled_back() {
        let s = setup(true);
        let mut host = Fake::new(&s.app);
        host.broken = Some("0.2.0".into());
        let out = run(&s.home, &host, Duration::ZERO).unwrap();
        assert_eq!(out.phase, Phase::RolledBack);
        assert!(out
            .error
            .as_ref()
            .unwrap()
            .contains("Quark 0.2.0 didn't start"));
        // The log and the updater's last line name what's installed now.
        assert_eq!(out.installed(), "0.1.0");
        assert_eq!(version(&s.app), "0.1.0");
        assert_eq!(
            *host.log.borrow(),
            [
                "quit",
                "restart 0.2.0",
                "restart 0.1.0",
                "open desktop=true"
            ]
        );
        // The old build couldn't read the new data, so the backup comes back.
        assert!(s.home.join("backups/restore").is_file());
    }

    #[test]
    fn a_wrong_version_answering_fails_the_gate() {
        let s = setup(false);
        let host = Fake::new(&s.app);
        // The old quarkd is still answering when the gate closes.
        host.answers.borrow_mut().extend([Some("0.1.0".into())]);
        let r = wait_healthy(&host, "u", "0.2.0", Duration::ZERO);
        assert_eq!(r, Err("quarkd 0.1.0 answered instead of 0.2.0".into()));
        // One healthy answer isn't enough; two are.
        *host.running.borrow_mut() = Some("0.2.0".into());
        host.answers
            .borrow_mut()
            .extend([Some("0.2.0".into()), None]);
        assert!(wait_healthy(&host, "u", "0.2.0", Duration::from_secs(60)).is_ok());
    }

    #[test]
    fn a_quarkd_that_keeps_dying_fails_the_gate_as_such() {
        let s = setup(false);
        let host = Fake::new(&s.app);
        host.answers
            .borrow_mut()
            .extend([Some("0.2.0".into()), None]);
        let r = wait_healthy(&host, "u", "0.2.0", Duration::ZERO);
        assert_eq!(
            r,
            Err("quarkd 0.2.0 didn't stay up for 2 checks in a row within 0s".into())
        );
    }

    #[test]
    fn when_neither_version_starts_the_install_fails() {
        let s = setup(false);
        let mut host = Fake::new(&s.app);
        host.broken = Some("0.2.0".into());
        *host.answers.borrow_mut() = VecDeque::new();
        // Make the old one broken too, after the first restart.
        struct Neither(Fake);
        impl Host for Neither {
            fn quit_apps(&self, a: &Path) -> anyhow::Result<bool> {
                self.0.quit_apps(a)
            }
            fn restart_quarkd(&self) -> anyhow::Result<()> {
                self.0.restart_quarkd()?;
                *self.0.running.borrow_mut() = None;
                Ok(())
            }
            fn open_apps(&self, a: &Path, d: bool) {
                self.0.open_apps(a, d)
            }
            fn health_version(&self, u: &str) -> Option<String> {
                self.0.health_version(u)
            }
            fn sleep(&self, _: Duration) {}
        }
        let r = run(&s.home, &Neither(host), Duration::ZERO);
        let e = format!("{:#}", r.unwrap_err());
        assert!(e.contains("didn't start either"), "{e}");
        let i = Intent::load(&s.home).unwrap();
        assert_eq!(i.phase, Phase::Failed);
        assert!(i.finished_at.is_some());
    }

    #[test]
    fn a_bad_bundle_leaves_the_app_alone() {
        let s = setup(false);
        let mut i = Intent::load(&s.home).unwrap();
        i.bundle = s.home.join("missing.tar.gz");
        i.save(&s.home).unwrap();
        let host = Fake::new(&s.app);
        assert!(run(&s.home, &host, Duration::ZERO).is_err());
        assert_eq!(version(&s.app), "0.1.0");
        assert!(host.log.borrow().is_empty());
        assert_eq!(Intent::load(&s.home).unwrap().phase, Phase::Failed);
    }

    #[test]
    fn finds_the_installed_app_from_quarkd() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("Applications/Quark.app");
        make_app(&app, "0.1.0");
        let exe = app.join("Contents/Library/LoginItems/QuarkBar.app/Contents/MacOS/quarkd");
        assert_eq!(installed_app(&exe), Some(app));
        assert_eq!(installed_app(Path::new("/usr/local/bin/quarkd")), None);
    }
}
