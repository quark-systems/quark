//! The pool's on-disk state, read and written exactly as treehouse 3.1.2
//! does, so treehouse and the native pool can take turns on one pool.
//!
//! A pool directory holds:
//!
//! - `treehouse-state.json`: `{"version": 4, "worktrees": [...]}`, replaced
//!   atomically (temp file, fsync, rename, directory fsync);
//! - `treehouse-state.key`: 32 random bytes keying the HMAC-SHA256 digest
//!   that authenticates each entry's seeded-file inventory;
//! - `treehouse-state.lock`: `flock`ed exclusively around every
//!   read-modify-write, by both implementations.
//!
//! Reading fails safe the way treehouse does: an entry whose inventory does
//! not authenticate, worktree directories missing from the file, and a file
//! that does not parse all come back leased to [`RECOVERED_HOLDER`], so
//! nothing is handed out, reset or removed until a person looks.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use hmac::{Hmac, Mac};
use quark_core::{CoreError, Result};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// The state format this module reads and writes.
pub const STATE_VERSION: u32 = 4;

/// Holder of an entry whose reservation could not be trusted.
pub const RECOVERED_HOLDER: &str =
    "recovered: state file was corrupt or truncated; verify before reuse";
/// Holder while an acquisition is between its reset and its lease.
pub const INCOMPLETE_HOLDER: &str = "quarantined: acquisition state incomplete";

const STATE_FILE: &str = "treehouse-state.json";
const KEY_FILE: &str = "treehouse-state.key";
const LOCK_FILE: &str = "treehouse-state.lock";
const KEY_LEN: usize = 32;

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// One slot, field for field as treehouse writes it. Timestamps stay as the
/// text treehouse wrote; fields this version does not know are kept.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    #[serde(default = "zero_time")]
    pub created_at: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub destroying: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub owner_pid: i64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub owner_started_at: i64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub leased: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lease_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lease_holder: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leased_at: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_branch: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seeded_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub seed_inventory_known: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub seed_inventory_digest: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub seed_backend: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub seed_auth_identity: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub recovery_error: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub recovery_reason: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn zero_time() -> String {
    "0001-01-01T00:00:00Z".into()
}

/// Now, as treehouse writes times (RFC 3339 with nanoseconds).
pub fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| zero_time())
}

impl Entry {
    /// A fresh entry, quarantined until its acquisition completes.
    pub fn new(name: &str, path: &Path, base_branch: &str) -> Self {
        let mut e = Self {
            name: name.into(),
            path: path.into(),
            created_at: now(),
            base_branch: base_branch.into(),
            ..Self::default()
        };
        e.quarantine(INCOMPLETE_HOLDER);
        e.set_seed_inventory_empty();
        e
    }

    /// Leased away from everyone, for `holder` (one of treehouse's
    /// `quarantined: ...` reasons).
    pub fn quarantine(&mut self, holder: &str) {
        self.leased = true;
        self.lease_holder = holder.into();
        self.leased_at = Some(now());
    }

    pub fn clear_lease(&mut self) {
        self.leased = false;
        self.lease_id.clear();
        self.lease_holder.clear();
        self.leased_at = None;
        self.recovery_reason.clear();
    }

    /// Back in the pool: no reservation, no lease, nothing seeded.
    pub fn release(&mut self) {
        self.owner_pid = 0;
        self.owner_started_at = 0;
        self.clear_lease();
        self.set_seed_inventory_empty();
    }

    /// A verified-empty seed inventory; the native pool seeds nothing.
    pub fn set_seed_inventory_empty(&mut self) {
        self.seeded_paths.clear();
        self.seed_inventory_known = true;
        self.seed_inventory_digest.clear();
        self.seed_backend.clear();
        self.seed_auth_identity.clear();
    }

    /// Durably leased to `holder` under a fresh lease id.
    pub fn lease(&mut self, holder: &str) -> Result<()> {
        self.leased = true;
        self.lease_id = new_lease_id()?;
        self.lease_holder = holder.into();
        self.leased_at = Some(now());
        self.owner_pid = 0;
        self.owner_started_at = 0;
        Ok(())
    }

    fn has_seed_state(&self) -> bool {
        self.seed_inventory_known
            || !self.seeded_paths.is_empty()
            || !self.seed_inventory_digest.is_empty()
            || !self.seed_backend.is_empty()
            || !self.seed_auth_identity.is_empty()
    }

    fn mark_recovered(&mut self) {
        self.leased = true;
        self.lease_holder = RECOVERED_HOLDER.into();
        self.seeded_paths.clear();
        self.seed_inventory_known = false;
        self.seed_inventory_digest.clear();
        self.seed_backend.clear();
        self.seed_auth_identity.clear();
        if self.leased_at.is_none() {
            self.leased_at = Some(now());
        }
    }

    fn recovered(name: &str, path: &Path, error: String) -> Self {
        let t = now();
        Self {
            name: name.into(),
            path: path.into(),
            created_at: t.clone(),
            leased: true,
            lease_holder: RECOVERED_HOLDER.into(),
            leased_at: Some(t),
            recovery_error: error,
            ..Self::default()
        }
    }
}

/// The whole state file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub version: u32,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub worktrees: Vec<Entry>,
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

fn null_as_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Entry>, D::Error> {
    Ok(Option::<Vec<Entry>>::deserialize(d)?.unwrap_or_default())
}

pub fn state_path(pool: &Path) -> PathBuf {
    pool.join(STATE_FILE)
}

/// `pool` holds a state file.
pub fn is_pool_dir(pool: &Path) -> bool {
    state_path(pool).exists()
}

fn io(what: &str, path: &Path, e: std::io::Error) -> CoreError {
    CoreError::Backend(format!("{what} {}: {e}", path.display()))
}

/// Read the pool's state, failing safe as treehouse does.
pub fn read(pool: &Path) -> Result<State> {
    let path = state_path(pool);
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !pool.exists() {
                return Ok(State::default());
            }
            return recover_missing(pool, State::default());
        }
        Err(e) => return Err(io("reading", &path, e)),
    };
    let mut state: State = match serde_json::from_slice(&data) {
        Ok(s) => s,
        Err(_) => return recover_corrupt(pool),
    };
    if state.version > STATE_VERSION {
        return Err(CoreError::Unsupported(format!(
            "unsupported treehouse state version {}",
            state.version
        )));
    }
    let key = read_key(pool);
    let key_absent = matches!(&key, Err(e) if e.kind() == std::io::ErrorKind::NotFound);
    let legacy = state.version == 0 && (key.is_ok() || key_absent);
    for wt in &mut state.worktrees {
        if legacy && !wt.has_seed_state() {
            wt.set_seed_inventory_empty();
            continue;
        }
        let valid =
            state.version == STATE_VERSION && key.as_ref().is_ok_and(|k| valid_digest(k, wt));
        if !valid {
            wt.mark_recovered();
        }
    }
    state.version = STATE_VERSION;
    recover_missing(pool, state)
}

/// Worktree directories on disk that the state does not list come back
/// quarantined (a crash between creating one and recording it).
fn recover_missing(pool: &Path, mut state: State) -> Result<State> {
    let known: Vec<PathBuf> = state.worktrees.iter().map(|w| clean(&w.path)).collect();
    for (slot, wt) in scan(pool)? {
        if known.contains(&clean(&wt)) {
            continue;
        }
        if let Some(e) = recover_one(&slot, &wt) {
            state.worktrees.push(e);
        }
    }
    Ok(state)
}

fn recover_corrupt(pool: &Path) -> Result<State> {
    let mut worktrees = Vec::new();
    for (slot, wt) in scan(pool)? {
        if let Some(e) = recover_one(&slot, &wt) {
            worktrees.push(e);
        }
    }
    Ok(State {
        version: 0,
        worktrees,
    })
}

/// Every `<pool>/<slot>/<dir>` directory, in directory order.
fn scan(pool: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for slot in std::fs::read_dir(pool).map_err(|e| io("scanning", pool, e))? {
        let slot = slot.map_err(|e| io("scanning", pool, e))?;
        if !slot.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let dir = slot.path();
        let name = slot.file_name().to_string_lossy().into_owned();
        let mut nested: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| io("scanning pool slot", &dir, e))?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        nested.sort();
        out.extend(nested.into_iter().map(|p| (name.clone(), p)));
    }
    out.sort();
    Ok(out)
}

fn recover_one(slot: &str, wt: &Path) -> Option<Entry> {
    match marker(wt) {
        Ok(None) => None,
        Ok(Some(_)) => Some(Entry::recovered(slot, wt, String::new())),
        Err(e) => Some(Entry::recovered(slot, wt, e)),
    }
}

/// Which VCS a slot's own marker names: `.git` (file or directory) is git,
/// a `.jj` directory is jj. An entry that exists but cannot be resolved
/// (a dangling symlink) is an error, not a missing marker.
pub fn marker(path: &Path) -> std::result::Result<Option<&'static str>, String> {
    let git = path.join(".git");
    match std::fs::symlink_metadata(&git) {
        Ok(_) => {
            return std::fs::metadata(&git)
                .map(|_| Some("git"))
                .map_err(|e| format!("resolving .git marker in {}: {e}", path.display()));
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
        Err(_) => {}
    }
    let jj = path.join(".jj");
    match std::fs::symlink_metadata(&jj) {
        Ok(_) => match std::fs::metadata(&jj) {
            Ok(m) if m.is_dir() => Ok(Some("jj")),
            Ok(_) => Ok(None),
            Err(e) => Err(format!("resolving .jj marker in {}: {e}", path.display())),
        },
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        Err(_) => Ok(None),
    }
}

/// Lexically cleaned path, as Go's `filepath.Clean`.
pub fn clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

fn read_key(pool: &Path) -> std::io::Result<Vec<u8>> {
    let key = std::fs::read(pool.join(KEY_FILE))?;
    if key.len() != KEY_LEN {
        return Err(std::io::Error::other("invalid treehouse state key"));
    }
    Ok(key)
}

fn ensure_key(pool: &Path) -> Result<Vec<u8>> {
    match read_key(pool) {
        Ok(k) => return Ok(k),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(io("reading", &pool.join(KEY_FILE), e))
        }
        Err(_) => {}
    }
    let mut key = vec![0u8; KEY_LEN];
    getrandom::fill(&mut key).map_err(|e| CoreError::Backend(format!("random key: {e}")))?;
    let path = pool.join(KEY_FILE);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut f) => {
            f.write_all(&key)
                .and_then(|_| f.sync_all())
                .map_err(|e| io("writing", &path, e))?;
            Ok(key)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            read_key(pool).map_err(|e| io("reading", &path, e))
        }
        Err(e) => Err(io("creating", &path, e)),
    }
}

/// The HMAC-SHA256 treehouse keeps per entry over `{name, path,
/// seeded_paths, seed_backend, seed_auth_identity}`, as Go's
/// `json.Marshal` would encode that struct.
pub fn digest(key: &[u8], wt: &Entry) -> String {
    let mut data = String::from("{\"name\":");
    go_string(&mut data, &wt.name);
    data.push_str(",\"path\":");
    go_string(&mut data, &clean(&wt.path).to_string_lossy());
    data.push_str(",\"seeded_paths\":[");
    for (i, p) in wt.seeded_paths.iter().enumerate() {
        if i > 0 {
            data.push(',');
        }
        go_string(&mut data, p);
    }
    data.push_str("],\"seed_backend\":");
    go_string(&mut data, &wt.seed_backend);
    data.push_str(",\"seed_auth_identity\":");
    go_string(&mut data, &wt.seed_auth_identity);
    data.push('}');
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A JSON string as Go's `encoding/json` writes it, HTML-safe escapes
/// included.
fn go_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn valid_digest(key: &[u8], wt: &Entry) -> bool {
    wt.seed_inventory_known
        && valid_inventory(&wt.seeded_paths)
        && valid_seed_metadata(wt)
        && hmac_eq(&wt.seed_inventory_digest, &digest(key, wt))
}

fn hmac_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn valid_seed_metadata(wt: &Entry) -> bool {
    if wt.seeded_paths.is_empty() {
        return wt.seed_backend.is_empty() && wt.seed_auth_identity.is_empty();
    }
    (wt.seed_backend == "git" && wt.seed_auth_identity.is_empty())
        || (wt.seed_backend == "jj" && !wt.seed_auth_identity.is_empty())
}

fn valid_inventory(paths: &[String]) -> bool {
    paths.iter().all(|name| {
        let first = name.split('/').next().unwrap_or_default();
        !name.is_empty()
            && !name.starts_with('/')
            && clean(Path::new(name)) == Path::new(name)
            && !name.ends_with('/')
            && !name.contains(['\\', '\0'])
            && !first.eq_ignore_ascii_case(".git")
            && !first.eq_ignore_ascii_case(".jj")
    })
}

/// Write `state` atomically, signing each entry's inventory.
pub fn write(pool: &Path, mut state: State) -> Result<()> {
    let key = ensure_key(pool)?;
    for wt in &mut state.worktrees {
        if wt.seed_inventory_known {
            if !valid_inventory(&wt.seeded_paths) || !valid_seed_metadata(wt) {
                return Err(CoreError::Backend("invalid seeded path inventory".into()));
            }
            wt.seed_inventory_digest = digest(&key, wt);
        } else {
            wt.seed_inventory_digest.clear();
        }
    }
    state.version = STATE_VERSION;
    let data = serde_json::to_vec_pretty(&state)
        .map_err(|e| CoreError::Backend(format!("encoding pool state: {e}")))?;
    atomic_write(&state_path(pool), &data)
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mode = std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o644);
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|e| CoreError::Backend(format!("random: {e}")))?;
    let name = format!(
        "{}.tmp-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        suffix
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let tmp = dir.join(name);
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(data)?;
        f.set_permissions(std::fs::Permissions::from_mode(mode))?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(|e| io("writing", path, e))
}

/// The pool's exclusive lock, released on drop. Treehouse takes the same
/// `flock`, so the two never interleave.
pub struct Lock(File);

impl Lock {
    pub fn take(pool: &Path) -> Result<Self> {
        std::fs::create_dir_all(pool).map_err(|e| io("creating", pool, e))?;
        let path = pool.join(LOCK_FILE);
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o644)
            .open(&path)
            .map_err(|e| io("opening", &path, e))?;
        // SAFETY: flock on a file descriptor this `File` owns.
        let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(io("locking", &path, std::io::Error::last_os_error()));
        }
        Ok(Self(f))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // SAFETY: as in `take`.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// 16 random bytes, hex: one acquisition's identity.
pub fn new_lease_id() -> Result<String> {
    let mut id = [0u8; 16];
    getrandom::fill(&mut id).map_err(|e| CoreError::Backend(format!("lease id: {e}")))?;
    Ok(id.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_json_strings() {
        let mut s = String::new();
        go_string(&mut s, "a<b>&\"c\\\n\u{1}\u{e9}\u{2028}");
        let u = |hex: &str| format!("{}u{hex}", '\\');
        let want = format!(
            "\"a{}b{}{}\\\"c\\\\\\n{}\u{e9}{}\"",
            u("003c"),
            u("003e"),
            u("0026"),
            u("0001"),
            u("2028")
        );
        assert_eq!(s, want);
    }

    #[test]
    fn entries_round_trip_unknown_fields() {
        let json = r#"{"name":"1","path":"/p/1/r","created_at":"2026-10-07T01:00:00.5-07:00","leased":true,"lease_id":"ab","future_field":{"x":1}}"#;
        let e: Entry = serde_json::from_str(json).unwrap();
        assert!(e.leased);
        assert_eq!(e.extra["future_field"]["x"], 1);
        let back = serde_json::to_value(&e).unwrap();
        assert_eq!(back["future_field"]["x"], 1);
        assert_eq!(back["created_at"], "2026-10-07T01:00:00.5-07:00");
        assert!(back.get("owner_pid").is_none());
        assert!(back.get("leased_at").is_none());
    }

    #[test]
    fn digests_authenticate_and_tampering_quarantines() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = tmp.path();
        let mut e = Entry::new("1", &pool.join("1/r"), "");
        e.release();
        write(
            pool,
            State {
                version: 0,
                worktrees: vec![e],
            },
        )
        .unwrap();
        let wt = pool.join("1/r");
        std::fs::create_dir_all(wt.join(".git")).unwrap();
        let s = read(pool).unwrap();
        assert!(!s.worktrees[0].leased, "{s:?}");

        let raw = std::fs::read_to_string(state_path(pool)).unwrap();
        std::fs::write(
            state_path(pool),
            raw.replace("\"name\": \"1\"", "\"name\": \"2\""),
        )
        .unwrap();
        let s = read(pool).unwrap();
        assert_eq!(s.worktrees[0].lease_holder, RECOVERED_HOLDER);
    }

    #[test]
    fn corrupt_and_missing_state_recover_quarantined() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = tmp.path();
        std::fs::create_dir_all(pool.join("1/r/.git")).unwrap();
        std::fs::create_dir_all(pool.join("2/r")).unwrap(); // no marker
        let s = read(pool).unwrap();
        assert_eq!(s.worktrees.len(), 1);
        assert_eq!(s.worktrees[0].lease_holder, RECOVERED_HOLDER);
        std::fs::write(state_path(pool), "{\"worktrees\": [").unwrap();
        let s = read(pool).unwrap();
        assert_eq!(s.worktrees.len(), 1);
        assert!(s.worktrees[0].leased);
    }

    #[test]
    fn newer_state_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(state_path(tmp.path()), r#"{"version":5,"worktrees":[]}"#).unwrap();
        assert!(matches!(read(tmp.path()), Err(CoreError::Unsupported(_))));
    }
}
