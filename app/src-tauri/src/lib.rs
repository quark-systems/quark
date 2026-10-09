// The webview talks to quarkd directly over HTTP and WebSocket, so the Rust side only
// opens the window and the system folder dialog (for local paths on New project). Two environment variables help automation and development:
// - QUARK_DAEMON points the app at a daemon other than http://127.0.0.1:7380.
// - QUARK_QUERY is appended to the page URL as-is (e.g. "renderer=webgl").
// - QUARK_GLASS=0 keeps the macOS window opaque.
//
// On macOS the window is translucent with a native blur behind it (an NSVisualEffectView
// through Tauri's window effects) and the title bar overlays the page; the page learns that
// from `glass=1&platform=macos` and paints its panels partly transparent. The approach is
// adapted from MonoCode (https://github.com/hardbeat920/monocode), MIT, Copyright (c) 2026 Nick.
use tauri::{WebviewUrl, WebviewWindowBuilder};

/// Query flags describing the window to the page.
fn shell_flags() -> Vec<String> {
    let mut flags = Vec::new();
    if cfg!(target_os = "macos") {
        flags.push("platform=macos".to_string());
        if glass_enabled() {
            flags.push("glass=1".to_string());
        }
    }
    flags
}

fn glass_enabled() -> bool {
    std::env::var("QUARK_GLASS").map_or(true, |v| v != "0")
}

fn page_path(shell: Vec<String>) -> String {
    let mut query: Vec<String> = shell;
    if let Ok(d) = std::env::var("QUARK_DAEMON") {
        if !d.is_empty() {
            query.push(format!("daemon={}", encode(&d)));
        }
    }
    if let Ok(q) = std::env::var("QUARK_QUERY") {
        if !q.is_empty() {
            query.push(q);
        }
    }
    if query.is_empty() {
        "index.html".to_string()
    } else {
        format!("index.html?{}", query.join("&"))
    }
}

/// Percent-encodes a query value.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let builder = WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::App(page_path(shell_flags()).into()),
            )
            .title("Quark")
            .inner_size(1440.0, 900.0)
            .min_inner_size(960.0, 600.0);
            #[cfg(target_os = "macos")]
            let builder = {
                use tauri::window::{Effect, EffectState, EffectsBuilder};
                let builder = builder
                    .title_bar_style(tauri::TitleBarStyle::Overlay)
                    .hidden_title(true);
                if glass_enabled() {
                    builder.transparent(true).effects(
                        EffectsBuilder::new()
                            .effect(Effect::UnderWindowBackground)
                            .state(EffectState::Active)
                            .build(),
                    )
                } else {
                    builder
                }
            };
            builder.build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Quark desktop app");
}

#[cfg(test)]
mod tests {
    use super::{encode, page_path};

    #[test]
    fn encodes_daemon_urls() {
        assert_eq!(
            encode("http://127.0.0.1:7380"),
            "http%3A%2F%2F127.0.0.1%3A7380"
        );
    }

    #[test]
    fn passes_shell_flags_to_the_page() {
        let p = page_path(vec!["platform=macos".into(), "glass=1".into()]);
        assert!(p.starts_with("index.html?platform=macos&glass=1"), "{p}");
    }
}
