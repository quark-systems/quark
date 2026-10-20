//! Getting a harness past its first-run questions in a folder, before an
//! agent is started there.
//!
//! Claude Code opens a folder it has not seen on "Do you trust this
//! folder?", and a first run with `--dangerously-skip-permissions` on a
//! bypass-permissions warning. Both default to "No, exit", so a brief or a
//! message typed in with Enter quits the agent. [`prepare_folder`] records
//! both answers in the agent's config directory first, the same entries
//! Claude Code writes when a person accepts them: the folder's
//! `hasTrustDialogAccepted` in `.claude.json` and
//! `skipDangerousModePermissionPrompt` in `settings.json`. Nothing is
//! written into the folder itself, so a worker's worktree stays clean.
//!
//! IBM Bob asks before every read, write or edit outside its workspace,
//! whatever `--auto-approve` says, unless the person opted those tool groups
//! in; a worker writes its status file outside its worktree, so it would
//! stall on its first report. That opt-in is the person's trust boundary,
//! so [`launch_warnings`] only checks it, read-only.
//!
//! Other harnesses need nothing here yet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use quark_core::harness::HarnessManifest;
use serde_json::{json, Value};

/// The `[hooks] install` value of harnesses with Claude Code's config.
const CLAUDE: &str = "claude-settings";
const CONFIG_ENV: &str = "CLAUDE_CONFIG_DIR";
/// The `[hooks] install` value of IBM Bob.
const BOB: &str = "bob-settings";
/// The tool groups a Bob worker needs outside its workspace.
const BOB_GROUPS: [&str; 3] = ["read", "write", "edit"];

/// Record that `folder` is trusted and the bypass-permissions warning was
/// accepted, for the account `env` selects (its `CLAUDE_CONFIG_DIR`, else
/// the default under `home`). A no-op for harnesses that ask neither.
pub fn prepare_folder(
    m: &HarnessManifest,
    folder: &Path,
    env: &BTreeMap<String, String>,
    home: Option<&Path>,
) -> std::io::Result<()> {
    if m.hooks.as_ref().is_none_or(|h| h.install != CLAUDE) {
        return Ok(());
    }
    let (state, settings) = match env.get(CONFIG_ENV).filter(|d| !d.is_empty()) {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            (dir.join(".claude.json"), dir.join("settings.json"))
        }
        None => {
            let Some(home) = home else {
                return Ok(());
            };
            (
                home.join(".claude.json"),
                home.join(".claude").join("settings.json"),
            )
        }
    };
    let mut keys = vec![folder.to_string_lossy().into_owned()];
    if let Ok(real) = folder.canonicalize() {
        let real = real.to_string_lossy().into_owned();
        if !keys.contains(&real) {
            keys.push(real);
        }
    }
    edit_json(&state, |v| {
        for key in &keys {
            let p = &mut v["projects"][key.as_str()];
            if !p.is_object() {
                *p = json!({});
            }
            p["hasTrustDialogAccepted"] = json!(true);
        }
    })?;
    edit_json(&settings, |v| {
        v["skipDangerousModePermissionPrompt"] = json!(true);
    })
}

/// What a person should fix before `m` can work unattended under `home`,
/// one sentence each; empty when nothing is known to be missing.
pub fn launch_warnings(m: &HarnessManifest, home: Option<&Path>) -> Vec<String> {
    if m.hooks.as_ref().is_none_or(|h| h.install != BOB) {
        return Vec::new();
    }
    let Some(home) = home else {
        return Vec::new();
    };
    let path = home.join(".bob").join("settings").join("settings.json");
    let v: Value = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => {
            return vec![format!(
                "could not read {} ({e}), so IBM Bob may stop to ask before writing its status file outside the worktree",
                path.display()
            )]
        }
    };
    let opted: Vec<&str> = v["approval"]["permissionOptions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| o["enableOutsideWorkspace"] == true)
        .filter_map(|o| o["groupId"].as_str())
        .collect();
    let missing: Vec<&str> = BOB_GROUPS
        .into_iter()
        .filter(|g| !opted.contains(g))
        .collect();
    if missing.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "IBM Bob will stop to ask before touching files outside the worktree: add an \
         approval.permissionOptions entry {{\"groupId\": <group>, \"enableOutsideWorkspace\": true}} \
         for {} in {}",
        missing.join(", "),
        path.display()
    )]
}

/// Apply `edit` to the JSON object at `path` (an empty object when the file
/// is missing), writing it back atomically only when it changed. A file
/// that is not a JSON object is left alone with an error.
fn edit_json(path: &Path, edit: impl FnOnce(&mut Value)) -> std::io::Result<()> {
    let old = match std::fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let mut v: Value = match &old {
        Some(s) if !s.trim().is_empty() => serde_json::from_str(s).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })?,
        _ => json!({}),
    };
    if !v.is_object() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not a JSON object", path.display()),
        ));
    }
    let before = v.clone();
    edit(&mut v);
    if old.is_some() && v == before {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("quark-{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(&v)? + "\n")?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManifestRegistry;

    fn read(p: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn trusts_the_folder_in_the_accounts_config() {
        let reg = ManifestRegistry::builtin();
        let claude = reg.get("claude-code").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("ws");
        std::fs::create_dir_all(&folder).unwrap();
        let account = dir.path().join("account");
        std::fs::create_dir_all(&account).unwrap();
        std::fs::write(
            account.join(".claude.json"),
            r#"{"theme":"dark","projects":{"/other":{"allowedTools":[]}}}"#,
        )
        .unwrap();
        let env = BTreeMap::from([(
            CONFIG_ENV.to_string(),
            account.to_string_lossy().into_owned(),
        )]);
        prepare_folder(claude, &folder, &env, None).unwrap();
        let state = read(&account.join(".claude.json"));
        assert_eq!(state["theme"], "dark");
        assert_eq!(state["projects"]["/other"]["allowedTools"], json!([]));
        let key = folder.to_string_lossy();
        assert_eq!(
            state["projects"][key.as_ref()]["hasTrustDialogAccepted"],
            true
        );
        assert_eq!(
            read(&account.join("settings.json"))["skipDangerousModePermissionPrompt"],
            true
        );

        // The default account lives under the home directory.
        let home = dir.path().join("home");
        prepare_folder(claude, &folder, &BTreeMap::new(), Some(&home)).unwrap();
        assert_eq!(
            read(&home.join(".claude.json"))["projects"][key.as_ref()]["hasTrustDialogAccepted"],
            true
        );
        assert!(home.join(".claude/settings.json").is_file());
        assert!(!folder.join(".claude").exists());
    }

    #[test]
    fn bob_needs_the_outside_workspace_opt_in() {
        let reg = ManifestRegistry::builtin();
        let bob = reg.get("bob").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let w = launch_warnings(bob, Some(dir.path()));
        assert!(w[0].starts_with("could not read"), "{w:?}");
        let settings = dir.path().join(".bob/settings/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(
            &settings,
            r#"{"approval":{"permissionOptions":[{"groupId":"read","enableOutsideWorkspace":true},{"groupId":"edit","enableOutsideWorkspace":false}]}}"#,
        )
        .unwrap();
        let w = launch_warnings(bob, Some(dir.path()));
        assert!(w[0].contains("for write, edit in "), "{w:?}");
        std::fs::write(
            &settings,
            r#"{"approval":{"permissionOptions":[{"groupId":"read","enableOutsideWorkspace":true},{"groupId":"write","enableOutsideWorkspace":true},{"groupId":"edit","enableOutsideWorkspace":true}]}}"#,
        )
        .unwrap();
        assert!(launch_warnings(bob, Some(dir.path())).is_empty());
        assert!(launch_warnings(reg.get("kiro").unwrap(), Some(dir.path())).is_empty());
    }

    #[test]
    fn other_harnesses_are_left_alone() {
        let reg = ManifestRegistry::builtin();
        let dir = tempfile::tempdir().unwrap();
        prepare_folder(
            reg.get("codex").unwrap(),
            dir.path(),
            &BTreeMap::new(),
            Some(dir.path()),
        )
        .unwrap();
        assert!(!dir.path().join(".claude.json").exists());
    }
}
