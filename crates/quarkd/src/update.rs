//! quarkd's updater: checks the update feed, downloads and verifies the
//! update bundle, and reports what it found on `/v1/update`.
//!
//! quarkd owns updates for the whole Mac install (Quark.app, QuarkBar and
//! quarkd ship as one bundle), so the app and QuarkBar only show what this
//! reports. The feed is the `update-feed` release's channel manifests, built
//! by `packaging/update-feed.mjs`: Tauri's `latest.json` plus `compat`,
//! `min_from` and `pulled`. A bundle is verified against the minisign key
//! baked in at build time (`TAURI_UPDATER_PUBKEY`, the key Tauri signs
//! with), so a build without one never offers updates. Installing (the swap,
//! health gate and rollback) is `quark-updater`'s job, not this module's.
//!
//! Settings live in `<home>/update.json`; downloads in `<home>/updates/<version>/`.
//! See docs/development/releasing.md, Updates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::compat::Compat;
use crate::update_install::{Intent, Phase};

/// An install that hasn't finished after this long has stopped.
const STALE: time::Duration = time::Duration::minutes(10);

/// Where the channel manifests are published; `{channel}` is `stable` or `beta`.
pub const FEED: &str =
    "https://github.com/quark-systems/quark/releases/download/update-feed/{channel}.json";
/// Overrides [`FEED`] (any URL curl reads, `file://` included), for testing a feed.
pub const FEED_ENV: &str = "QUARK_UPDATE_FEED";
/// The first check after quarkd starts, then every [`CHECK_EVERY`].
pub const FIRST_CHECK: Duration = Duration::from_secs(60);
pub const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

/// The minisign public key update bundles are signed with: base64 of the key
/// file's text, as Tauri's `pubkey` takes it. Set by the release build.
pub const PUBKEY: Option<&str> = option_env!("TAURI_UPDATER_PUBKEY");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    #[default]
    Stable,
    Beta,
}

impl Channel {
    fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        }
    }
}

/// The user's update settings (`<home>/update.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct Settings {
    /// Which releases to follow: full releases, or pre-releases too.
    pub channel: Channel,
    /// Check the feed every six hours.
    pub check: bool,
    /// Download an update as soon as one is found.
    pub auto_download: bool,
    /// Install a downloaded update by itself once no worker is running.
    pub install_when_idle: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            channel: Channel::Stable,
            check: true,
            auto_download: true,
            install_when_idle: false,
        }
    }
}

impl Settings {
    fn path(home: &Path) -> PathBuf {
        home.join("update.json")
    }

    pub fn load(home: &Path) -> Settings {
        std::fs::read(Self::path(home))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        let path = Self::path(home);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
    }
}

/// A channel manifest from the feed.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub pub_date: Option<String>,
    #[serde(default)]
    pub platforms: HashMap<String, Platform>,
    #[serde(default)]
    pub compat: Option<Compat>,
    #[serde(default)]
    pub min_from: Option<String>,
    #[serde(default)]
    pub pulled: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Platform {
    /// Base64 of the bundle's minisign signature file.
    pub signature: String,
    pub url: String,
}

/// The feed's key for this machine's builds, as Tauri names them.
pub fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("darwin-aarch64"),
        ("macos", "x86_64") => Some("darwin-x86_64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        _ => None,
    }
}

/// An update this quarkd may install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Offer {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pub_date: Option<String>,
    /// This build was withdrawn, so the update is a fix, not just newer.
    pub replaces_pulled: bool,
    /// Rolling back after this update needs the pre-update backup, because
    /// this build can't read the data the update writes. `None` when the
    /// manifest doesn't say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollback_needs_restore: Option<bool>,
    #[serde(skip)]
    pub url: String,
    #[serde(skip)]
    pub signature: String,
}

/// What the feed means for a build at `current` on `platform`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    UpToDate,
    Available(Offer),
    /// Newer, but not reachable from here in one step.
    Blocked {
        version: String,
        reason: String,
    },
    /// Newer, but with no build for this platform.
    NoBuild {
        version: String,
    },
}

pub fn evaluate(m: &Manifest, current: &str, platform: &str) -> anyhow::Result<Verdict> {
    let cur = semver::Version::parse(current).with_context(|| format!("version {current}"))?;
    let new = semver::Version::parse(&m.version)
        .with_context(|| format!("feed version {}", m.version))?;
    let replaces_pulled = m.pulled.iter().any(|p| p == current);
    // A pulled build is offered whatever the feed serves, even an older one.
    if new == cur || (new < cur && !replaces_pulled) {
        return Ok(Verdict::UpToDate);
    }
    if let Some(min) = &m.min_from {
        let min = semver::Version::parse(min).with_context(|| format!("min_from {min}"))?;
        if cur < min {
            return Ok(Verdict::Blocked {
                version: m.version.clone(),
                reason: format!(
                    "Quark {} can't update from {current}. Install {min} or later from the release page first.",
                    m.version
                ),
            });
        }
    }
    let Some(p) = m.platforms.get(platform) else {
        return Ok(Verdict::NoBuild {
            version: m.version.clone(),
        });
    };
    let rollback_needs_restore = m
        .compat
        .as_ref()
        .map(|c| !c.readable_by(&Compat::current()));
    Ok(Verdict::Available(Offer {
        version: m.version.clone(),
        notes: m.notes.clone(),
        pub_date: m.pub_date.clone(),
        replaces_pulled,
        rollback_needs_restore,
        url: p.url.clone(),
        signature: p.signature.clone(),
    }))
}

/// Checks `data` against a Tauri-style signature: `pubkey` and `signature`
/// are base64 of minisign's key and signature files.
pub fn verify(pubkey: &str, signature: &str, data: &[u8]) -> anyhow::Result<()> {
    let text = |b64: &str, what: &str| -> anyhow::Result<String> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .with_context(|| format!("{what} is not base64"))?;
        String::from_utf8(bytes).with_context(|| format!("{what} is not text"))
    };
    let key = minisign_verify::PublicKey::decode(&text(pubkey, "public key")?)
        .map_err(|e| anyhow!("public key: {e}"))?;
    let sig = minisign_verify::Signature::decode(&text(signature, "signature")?)
        .map_err(|e| anyhow!("signature: {e}"))?;
    key.verify(data, &sig, true)
        .map_err(|e| anyhow!("signature doesn't match: {e}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// This build can't update itself (no signing key or no platform build).
    Disabled,
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    /// Downloaded and verified; ready to install.
    Ready,
    Failed,
    /// quark-updater is swapping the bundle; quarkd restarts shortly.
    Installing,
}

/// `GET /v1/update`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct UpdateStatus {
    pub state: State,
    pub current_version: String,
    /// Why updates are off, when `state` is `disabled`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offer: Option<Offer>,
    /// A newer release this one can't update to directly, and why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
    /// The last check or download's failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<String>,
    /// The verified bundle, once `state` is `ready`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
    /// The last install or rollback, or the one under way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_install: Option<crate::update_install::Intent>,
    /// Roll back is possible: an earlier Quark is kept beside this one.
    pub can_roll_back: bool,
    pub settings: Settings,
}

/// One home's updater. Shared by the periodic check and the API.
pub struct Updater {
    home: PathBuf,
    feed: String,
    pubkey: Option<String>,
    platform: Option<String>,
    current: String,
    /// quarkd's `/v1/health`, for the updater's health check.
    health_url: Mutex<String>,
    inner: Mutex<Inner>,
    /// One check or download at a time.
    busy: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Inner {
    state: Option<State>,
    offer: Option<Offer>,
    blocked: Option<String>,
    error: Option<String>,
    checked_at: Option<OffsetDateTime>,
    bundle: Option<PathBuf>,
}

impl Updater {
    pub fn new(
        home: PathBuf,
        feed: String,
        pubkey: Option<String>,
        platform: Option<String>,
        current: String,
    ) -> Self {
        Self {
            home,
            feed,
            pubkey,
            platform,
            current,
            health_url: Mutex::new("http://127.0.0.1:7380/v1/health".into()),
            inner: Mutex::new(Inner::default()),
            busy: tokio::sync::Mutex::new(()),
        }
    }

    /// The updater for `home`, made with this build's key, platform and feed.
    pub fn for_home(home: &Path) -> Arc<Updater> {
        static ALL: OnceLock<Mutex<HashMap<PathBuf, Arc<Updater>>>> = OnceLock::new();
        let mut all = ALL.get_or_init(Default::default).lock().unwrap();
        all.entry(home.to_path_buf())
            .or_insert_with(|| {
                Arc::new(Updater::new(
                    home.to_path_buf(),
                    std::env::var(FEED_ENV).unwrap_or_else(|_| FEED.to_string()),
                    PUBKEY.filter(|k| !k.trim().is_empty()).map(str::to_string),
                    platform().map(str::to_string),
                    env!("CARGO_PKG_VERSION").to_string(),
                ))
            })
            .clone()
    }

    fn disabled_reason(&self) -> Option<String> {
        if self.pubkey.is_none() {
            Some("This build of quarkd has no update signing key.".into())
        } else if self.platform.is_none() {
            Some("There are no update builds for this platform.".into())
        } else {
            None
        }
    }

    /// Where quarkd serves `/v1/health`, from its listen address.
    pub fn set_listen(&self, listen: std::net::SocketAddr) {
        *self.health_url.lock().unwrap() = format!("http://{listen}/v1/health");
    }

    /// The last install's record. One that never finished (the updater was
    /// killed, or the Mac restarted) reads as failed after [`STALE`].
    fn last_install(&self) -> Option<Intent> {
        let mut i = Intent::load(&self.home)?;
        let started = time::OffsetDateTime::parse(
            &i.started_at,
            &time::format_description::well_known::Rfc3339,
        )
        .ok()?;
        if i.in_progress() && OffsetDateTime::now_utc() - started > STALE {
            i.error = Some(format!(
                "the update stopped while {:?}; this is Quark {}",
                i.phase, self.current
            ));
            i.phase = Phase::Failed;
        }
        Some(i)
    }

    /// The installed Quark.app, when quarkd runs from one.
    fn app(&self) -> Option<PathBuf> {
        crate::update_install::installed_app(&std::env::current_exe().ok()?)
    }

    fn can_roll_back(&self, last: Option<&Intent>) -> bool {
        let Some(last) = last else { return false };
        !last.rollback
            && last.phase == Phase::Done
            && last.to == self.current
            && last
                .app
                .parent()
                .is_some_and(|p| p.join(crate::update_install::PREVIOUS).is_dir())
    }

    /// Installs the downloaded update: backs up the home, then starts
    /// quark-updater ([`crate::update_install`]), which restarts quarkd.
    pub fn install(&self) -> anyhow::Result<UpdateStatus> {
        let (offer, bundle) = {
            let i = self.inner.lock().unwrap();
            match (&i.state, &i.offer, &i.bundle) {
                (Some(State::Ready), Some(o), Some(b)) => (o.clone(), b.clone()),
                _ => bail!("no update is downloaded and ready to install"),
            }
        };
        let app = self
            .app()
            .ok_or_else(|| anyhow!("only quarkd inside an installed Quark.app can update it"))?;
        self.start(Intent {
            from: self.current.clone(),
            to: offer.version.clone(),
            rollback: false,
            app,
            bundle,
            backup: None,
            // Unknown means restore: losing minutes of data beats a quarkd
            // that can't open its home.
            restore_on_rollback: offer.rollback_needs_restore.unwrap_or(true),
            health_url: self.health_url.lock().unwrap().clone(),
            phase: Phase::Pending,
            started_at: crate::update_install::now(),
            finished_at: None,
            error: None,
        })
    }

    /// Goes back to the Quark kept aside by the last update.
    pub fn roll_back(&self) -> anyhow::Result<UpdateStatus> {
        let last = self.last_install();
        anyhow::ensure!(
            self.can_roll_back(last.as_ref()),
            "there is no earlier Quark to roll back to"
        );
        let last = last.unwrap();
        self.start(Intent {
            from: self.current.clone(),
            to: last.from.clone(),
            rollback: true,
            app: last.app.clone(),
            bundle: PathBuf::new(),
            backup: last.backup.clone(),
            restore_on_rollback: last.restore_on_rollback,
            health_url: self.health_url.lock().unwrap().clone(),
            phase: Phase::Pending,
            started_at: crate::update_install::now(),
            finished_at: None,
            error: None,
        })
    }

    fn start(&self, mut intent: Intent) -> anyhow::Result<UpdateStatus> {
        if let Some(l) = self.last_install() {
            anyhow::ensure!(!l.in_progress(), "an update is already being installed");
        }
        if !intent.rollback {
            intent.backup = Some(
                crate::backup::snapshot(&self.home, &intent.to).context("backing up the home")?,
            );
        }
        let updater = self.home.join("updates").join("quark-updater");
        std::fs::create_dir_all(updater.parent().unwrap())?;
        let _ = std::fs::remove_file(&updater);
        std::fs::copy(std::env::current_exe()?, &updater).context("copying quark-updater")?;
        intent.save(&self.home)?;
        tracing::info!(from = %intent.from, to = %intent.to, rollback = intent.rollback, "update install starting");
        let log = self.home.join("logs").join("updater.log");
        std::fs::create_dir_all(log.parent().unwrap())?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)?;
        use std::os::unix::process::CommandExt;
        let spawned = std::process::Command::new(&updater)
            .arg("update-install")
            .arg("--home")
            .arg(&self.home)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            // Its own process group, so launchd stopping quarkd leaves it be.
            .process_group(0)
            .spawn();
        if let Err(e) = spawned {
            intent.phase = Phase::Failed;
            intent.error = Some(format!("starting quark-updater: {e}"));
            intent.finished_at = Some(crate::update_install::now());
            intent.save(&self.home)?;
            bail!("starting quark-updater: {e}");
        }
        Ok(self.status())
    }

    pub fn settings(&self) -> Settings {
        Settings::load(&self.home)
    }

    pub fn set_settings(&self, settings: &Settings) -> anyhow::Result<()> {
        settings.save(&self.home)
    }

    pub fn status(&self) -> UpdateStatus {
        let disabled_reason = self.disabled_reason();
        let last = self.last_install();
        let i = self.inner.lock().unwrap();
        let state = if disabled_reason.is_some() {
            State::Disabled
        } else if last.as_ref().is_some_and(|l| l.in_progress()) {
            State::Installing
        } else {
            i.state.unwrap_or(State::Idle)
        };
        UpdateStatus {
            state,
            current_version: self.current.clone(),
            disabled_reason,
            offer: i.offer.clone(),
            blocked: i.blocked.clone(),
            error: i.error.clone(),
            checked_at: i.checked_at.and_then(|t| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            }),
            bundle: i.bundle.as_ref().map(|p| p.display().to_string()),
            last_install: last.clone(),
            can_roll_back: self.can_roll_back(last.as_ref()),
            settings: self.settings(),
        }
    }

    fn set_state(&self, state: State) {
        self.inner.lock().unwrap().state = Some(state);
    }

    fn fail(&self, e: &anyhow::Error) {
        tracing::warn!(error = %format!("{e:#}"), "update");
        let mut i = self.inner.lock().unwrap();
        i.state = Some(State::Failed);
        i.error = Some(format!("{e:#}"));
    }

    /// Reads the feed, and downloads what it offers when the settings say to.
    pub async fn check(&self) -> UpdateStatus {
        if self.disabled_reason().is_some() {
            return self.status();
        }
        let _busy = self.busy.lock().await;
        self.set_state(State::Checking);
        match self.read_feed().await {
            Ok(verdict) => self.record(verdict),
            Err(e) => {
                self.fail(&e);
                return self.status();
            }
        }
        let ready = self.inner.lock().unwrap().state == Some(State::Ready);
        if !ready && self.offer().is_some() && self.settings().auto_download {
            self.download_locked().await;
        }
        self.status()
    }

    /// Downloads and verifies the offered update.
    pub async fn download(&self) -> UpdateStatus {
        if self.disabled_reason().is_some() {
            return self.status();
        }
        let _busy = self.busy.lock().await;
        self.download_locked().await;
        self.status()
    }

    fn offer(&self) -> Option<Offer> {
        self.inner.lock().unwrap().offer.clone()
    }

    async fn read_feed(&self) -> anyhow::Result<Verdict> {
        let url = self
            .feed
            .replace("{channel}", self.settings().channel.as_str());
        let body = fetch(&url).await?;
        let manifest: Manifest =
            serde_json::from_slice(&body).with_context(|| format!("reading {url}"))?;
        evaluate(
            &manifest,
            &self.current,
            self.platform.as_deref().unwrap_or_default(),
        )
    }

    fn record(&self, verdict: Verdict) {
        let mut i = self.inner.lock().unwrap();
        i.checked_at = Some(OffsetDateTime::now_utc());
        i.error = None;
        i.blocked = None;
        let (state, offer) = match verdict {
            Verdict::UpToDate => (State::UpToDate, None),
            Verdict::NoBuild { .. } => (State::UpToDate, None),
            Verdict::Blocked { reason, .. } => {
                i.blocked = Some(reason);
                (State::UpToDate, None)
            }
            Verdict::Available(offer) => {
                let bundle = bundle_path(&self.home, &offer);
                if i.offer.as_ref() == Some(&offer) && bundle.as_ref().is_some_and(|b| b.exists()) {
                    // Same offer, already downloaded and verified.
                    (State::Ready, Some(offer))
                } else {
                    i.bundle = None;
                    (State::Available, Some(offer))
                }
            }
        };
        if offer.is_none() {
            i.bundle = None;
        }
        tracing::info!(
            state = ?state,
            current = %self.current,
            offer = offer.as_ref().map(|o| o.version.as_str()).unwrap_or("-"),
            blocked = i.blocked.as_deref().unwrap_or("-"),
            "update check"
        );
        i.state = Some(state);
        i.offer = offer;
    }

    async fn download_locked(&self) {
        let Some(offer) = self.offer() else { return };
        let Some(pubkey) = self.pubkey.clone() else {
            return;
        };
        self.set_state(State::Downloading);
        match download(&self.home, &offer, &pubkey).await {
            Ok(path) => {
                tracing::info!(version = %offer.version, bundle = %path.display(), "update downloaded");
                let mut i = self.inner.lock().unwrap();
                i.state = Some(State::Ready);
                i.error = None;
                i.bundle = Some(path);
            }
            Err(e) => self.fail(&e),
        }
    }

    /// Checks soon after start and then every six hours, while the settings
    /// allow it.
    pub async fn run(self: Arc<Self>) {
        if let Some(reason) = self.disabled_reason() {
            tracing::info!(%reason, "updates off");
            return;
        }
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            if self.settings().check {
                self.check().await;
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    }
}

/// `<home>/updates/<version>/<bundle file name>`.
fn bundle_path(home: &Path, offer: &Offer) -> Option<PathBuf> {
    let name = offer
        .url
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty() && *n != "." && *n != ".." && !n.contains('\\'))?;
    semver::Version::parse(&offer.version).ok()?;
    Some(home.join("updates").join(&offer.version).join(name))
}

/// Downloads the offer's bundle, verifies it and keeps it under
/// `<home>/updates/<version>/`, removing older downloads.
async fn download(home: &Path, offer: &Offer, pubkey: &str) -> anyhow::Result<PathBuf> {
    let path = bundle_path(home, offer)
        .ok_or_else(|| anyhow!("can't name a download for {}", offer.url))?;
    let dir = path.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let part = path.with_extension("part");
    let out = tokio::process::Command::new("curl")
        .args(["-fsSL", "--retry", "2", "--max-time", "1800", "-o"])
        .arg(&part)
        .arg(&offer.url)
        .output()
        .await
        .context("running curl")?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&part);
        bail!(
            "downloading {}: {}",
            offer.url,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let data = tokio::fs::read(&part).await?;
    let checked = {
        let (pubkey, sig) = (pubkey.to_string(), offer.signature.clone());
        tokio::task::spawn_blocking(move || verify(&pubkey, &sig, &data)).await?
    };
    if let Err(e) = checked {
        let _ = std::fs::remove_file(&part);
        return Err(e.context(format!("verifying {}", offer.url)));
    }
    std::fs::rename(&part, &path)?;
    // Only the newest download is worth keeping.
    if let Ok(entries) = std::fs::read_dir(home.join("updates")) {
        for e in entries.flatten() {
            if e.path() != dir && e.file_type().is_ok_and(|t| t.is_dir()) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    Ok(path)
}

async fn fetch(url: &str) -> anyhow::Result<Vec<u8>> {
    let out = tokio::process::Command::new("curl")
        .args(["-fsSL", "--retry", "2", "--max-time", "60", url])
        .output()
        .await
        .context("running curl")?;
    if !out.status.success() {
        bail!(
            "reading {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    /// A key pair and a Tauri-style signer, as `tauri signer` makes them.
    struct Signer(minisign::KeyPair);

    impl Signer {
        fn new() -> Self {
            Signer(minisign::KeyPair::generate_unencrypted_keypair().unwrap())
        }
        fn pubkey(&self) -> String {
            b64(&self.0.pk.to_box().unwrap().to_string())
        }
        fn sign(&self, data: &[u8]) -> String {
            let sig = minisign::sign(
                Some(&self.0.pk),
                &self.0.sk,
                Cursor::new(data),
                Some("file:Quark.app.tar.gz"),
                None,
            )
            .unwrap();
            b64(&sig.to_string())
        }
    }

    fn manifest(version: &str) -> Manifest {
        serde_json::from_value(serde_json::json!({
            "version": version,
            "notes": "notes",
            "platforms": {"darwin-aarch64": {"signature": "c2ln", "url": "https://x/v/Quark.app.tar.gz"}},
            "min_from": "0.1.0",
            "pulled": [],
        }))
        .unwrap()
    }

    #[test]
    fn offers_only_a_newer_release() {
        let m = manifest("0.2.0");
        let Verdict::Available(o) = evaluate(&m, "0.1.0", "darwin-aarch64").unwrap() else {
            panic!("not offered")
        };
        assert_eq!(o.version, "0.2.0");
        assert!(!o.replaces_pulled);
        assert_eq!(o.rollback_needs_restore, None);
        assert_eq!(
            evaluate(&m, "0.2.0", "darwin-aarch64").unwrap(),
            Verdict::UpToDate
        );
        assert_eq!(
            evaluate(&m, "0.3.0", "darwin-aarch64").unwrap(),
            Verdict::UpToDate
        );
        // A pre-release sorts below its release.
        assert!(matches!(
            evaluate(&m, "0.2.0-beta.1", "darwin-aarch64").unwrap(),
            Verdict::Available(_)
        ));
        assert_eq!(
            evaluate(&m, "0.1.0", "linux-x86_64").unwrap(),
            Verdict::NoBuild {
                version: "0.2.0".into()
            }
        );
    }

    #[test]
    fn a_pulled_build_is_offered_the_feed_release_even_if_older() {
        let mut m = manifest("0.2.0");
        m.pulled = vec!["0.2.1".into()];
        let Verdict::Available(o) = evaluate(&m, "0.2.1", "darwin-aarch64").unwrap() else {
            panic!("not offered")
        };
        assert!(o.replaces_pulled);
    }

    #[test]
    fn min_from_blocks_a_long_jump() {
        let mut m = manifest("0.5.0");
        m.min_from = Some("0.3.0".into());
        let Verdict::Blocked { reason, .. } = evaluate(&m, "0.2.0", "darwin-aarch64").unwrap()
        else {
            panic!("not blocked")
        };
        assert!(reason.contains("Install 0.3.0 or later"), "{reason}");
    }

    #[test]
    fn rollback_needs_restore_when_this_build_cant_read_the_new_data() {
        let mut m = manifest("0.2.0");
        let mut c = Compat::current();
        m.compat = Some(c.clone());
        let Verdict::Available(o) = evaluate(&m, "0.1.0", "darwin-aarch64").unwrap() else {
            panic!()
        };
        assert_eq!(o.rollback_needs_restore, Some(false));
        c.store_min_reader = Compat::current().store_schema + 1;
        m.compat = Some(c);
        let Verdict::Available(o) = evaluate(&m, "0.1.0", "darwin-aarch64").unwrap() else {
            panic!()
        };
        assert_eq!(o.rollback_needs_restore, Some(true));
    }

    #[test]
    fn verifies_tauri_signatures() {
        let s = Signer::new();
        let sig = s.sign(b"bundle");
        verify(&s.pubkey(), &sig, b"bundle").unwrap();
        assert!(verify(&s.pubkey(), &sig, b"tampered").is_err());
        assert!(verify(&Signer::new().pubkey(), &sig, b"bundle").is_err());
        assert!(verify(&s.pubkey(), "not base64!", b"bundle").is_err());
    }

    #[test]
    fn settings_default_and_round_trip() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(home.path()), Settings::default());
        let s = Settings {
            channel: Channel::Beta,
            check: false,
            auto_download: false,
            install_when_idle: true,
        };
        s.save(home.path()).unwrap();
        assert_eq!(Settings::load(home.path()), s);
        // Missing keys take their defaults.
        std::fs::write(home.path().join("update.json"), r#"{"channel":"beta"}"#).unwrap();
        assert_eq!(
            Settings::load(home.path()),
            Settings {
                channel: Channel::Beta,
                ..Settings::default()
            }
        );
    }

    #[test]
    fn a_build_without_a_key_never_checks() {
        let home = tempfile::tempdir().unwrap();
        let u = Updater::new(
            home.path().into(),
            "file:///nowhere/{channel}.json".into(),
            None,
            Some("darwin-aarch64".into()),
            "0.1.0".into(),
        );
        assert_eq!(u.status().state, State::Disabled);
        let st = tokio::runtime::Runtime::new().unwrap().block_on(u.check());
        assert_eq!(st.state, State::Disabled);
        assert!(st.error.is_none());
    }

    /// A feed and bundle on disk, read through curl's file:// support.
    struct Feed {
        dir: tempfile::TempDir,
        signer: Signer,
    }

    impl Feed {
        fn new() -> Self {
            Feed {
                dir: tempfile::tempdir().unwrap(),
                signer: Signer::new(),
            }
        }
        fn publish(&self, version: &str, bundle: &[u8], signed: &[u8]) {
            let file = self.dir.path().join(format!("Quark-{version}.app.tar.gz"));
            std::fs::write(&file, bundle).unwrap();
            let m = serde_json::json!({
                "version": version,
                "platforms": {"darwin-aarch64": {
                    "signature": self.signer.sign(signed),
                    "url": format!("file://{}", file.display()),
                }},
                "min_from": "0.1.0",
                "pulled": [],
            });
            std::fs::write(self.dir.path().join("stable.json"), m.to_string()).unwrap();
        }
        fn updater(&self, home: &Path) -> Updater {
            Updater::new(
                home.into(),
                format!("file://{}/{{channel}}.json", self.dir.path().display()),
                Some(self.signer.pubkey()),
                Some("darwin-aarch64".into()),
                "0.1.0".into(),
            )
        }
    }

    #[tokio::test]
    async fn checks_downloads_and_verifies() {
        let feed = Feed::new();
        let home = tempfile::tempdir().unwrap();
        let u = feed.updater(home.path());
        feed.publish("0.1.0", b"same", b"same");
        assert_eq!(u.check().await.state, State::UpToDate);

        feed.publish("0.2.0", b"new bundle", b"new bundle");
        let st = u.check().await;
        assert_eq!(st.state, State::Ready, "{st:?}");
        let bundle = PathBuf::from(st.bundle.unwrap());
        assert_eq!(std::fs::read(&bundle).unwrap(), b"new bundle");
        assert!(bundle.starts_with(home.path().join("updates/0.2.0")));
        assert!(st.checked_at.is_some());
        // Checking again keeps the verified download.
        assert_eq!(u.check().await.state, State::Ready);

        // A newer release replaces the older download.
        feed.publish("0.3.0", b"newer", b"newer");
        let st = u.check().await;
        assert_eq!(st.state, State::Ready);
        assert!(!home.path().join("updates/0.2.0").exists());
    }

    #[tokio::test]
    async fn a_bad_signature_fails_and_keeps_nothing() {
        let feed = Feed::new();
        let home = tempfile::tempdir().unwrap();
        let u = feed.updater(home.path());
        feed.publish("0.2.0", b"tampered", b"original");
        let st = u.check().await;
        assert_eq!(st.state, State::Failed);
        assert!(st.error.unwrap().contains("signature"));
        assert!(st.bundle.is_none());
        let dir = home.path().join("updates/0.2.0");
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn without_auto_download_an_update_waits() {
        let feed = Feed::new();
        let home = tempfile::tempdir().unwrap();
        let u = feed.updater(home.path());
        u.set_settings(&Settings {
            auto_download: false,
            ..Settings::default()
        })
        .unwrap();
        feed.publish("0.2.0", b"b", b"b");
        let st = u.check().await;
        assert_eq!(st.state, State::Available);
        assert_eq!(st.offer.unwrap().version, "0.2.0");
        assert_eq!(u.download().await.state, State::Ready);
    }

    #[tokio::test]
    async fn an_unreachable_feed_fails() {
        let home = tempfile::tempdir().unwrap();
        let u = Updater::new(
            home.path().into(),
            format!("file://{}/missing/{{channel}}.json", home.path().display()),
            Some(Signer::new().pubkey()),
            Some("darwin-aarch64".into()),
            "0.1.0".into(),
        );
        let st = u.check().await;
        assert_eq!(st.state, State::Failed);
        assert!(st.error.unwrap().contains("missing"));
    }
}
