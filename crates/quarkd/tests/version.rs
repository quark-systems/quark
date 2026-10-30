//! Every file that carries Quark's release version agrees with the workspace's.
//! `scripts/bump-version.sh` sets them all; the release workflow checks the tag too.

use std::path::Path;

fn read(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The first `version = "..."` line of a Cargo.toml.
fn toml_version(text: &str) -> &str {
    text.lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("a version line")
}

fn json_version(text: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(text).unwrap();
    v["version"].as_str().expect("a version").to_string()
}

#[test]
fn release_version_is_the_same_everywhere() {
    let ours = env!("CARGO_PKG_VERSION");
    assert_eq!(toml_version(&read("Cargo.toml")), ours, "Cargo.toml");
    assert_eq!(
        toml_version(&read("app/src-tauri/Cargo.toml")),
        ours,
        "app/src-tauri/Cargo.toml"
    );
    assert_eq!(
        json_version(&read("app/package.json")),
        ours,
        "app/package.json"
    );
    assert_eq!(
        json_version(&read("app/src-tauri/tauri.conf.json")),
        ours,
        "app/src-tauri/tauri.conf.json"
    );
}
