// The webview talks to quarkd directly over HTTP and WebSocket, so the Rust side only
// opens the window and the system folder dialog (for local paths on New project). Two environment variables help automation and development:
// - QUARK_DAEMON points the app at a daemon other than http://127.0.0.1:7380.
// - QUARK_QUERY is appended to the page URL as-is (e.g. "renderer=webgl").
// - QUARK_GLASS=0 keeps the macOS window opaque.
//
// `quark://open/<route>` links (from QuarkBar, notifications and widgets) bring the window
// forward on that route: <route> is the page's own hash route (src/nav.ts), e.g.
// `quark://open/t/<task id>`. macOS only delivers them to the bundled app, so the app also
// takes them, one per line, on the socket `link_socket` names: QuarkBar writes there first,
// which reaches a desktop app run with `npm run tauri dev` too.
//
// On macOS the window is translucent with a native blur behind it (an NSVisualEffectView
// through Tauri's window effects) and the title bar overlays the page; the page learns that
// from `glass=1&platform=macos` and paints its panels partly transparent. The approach is
// adapted from MonoCode (https://github.com/hardbeat920/monocode), MIT, Copyright (c) 2026 Nick.
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

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

/// The page hash a `quark://open/<route>` link names, e.g. `#/t/tsk_1`. Anything else, or
/// a route with characters a hash route never holds, names none.
fn link_hash(url: &str) -> Option<String> {
    let route = url.strip_prefix("quark://open")?;
    let route = route.strip_prefix('/').unwrap_or(route);
    let route = route.split(['?', '#']).next().unwrap_or("");
    let ok = route
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/-_.~%".contains(&b));
    ok.then(|| format!("#/{route}"))
}

/// Shows the main window on the route `url` names.
fn open_link(app: &AppHandle, url: &str) {
    let (Some(hash), Some(window)) = (link_hash(url), app.get_webview_window("main")) else {
        return;
    };
    // `link_hash` only lets through characters that need no escaping in a JS string.
    let _ = window.eval(format!("location.hash = '{hash}'"));
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

/// The socket QuarkBar sends links to: ~/Library/Application Support/Quark/desktop.sock,
/// as mac/Sources/QuarkMenuBar/Opener.swift expects.
#[cfg(unix)]
fn link_socket(home: &std::path::Path) -> std::path::PathBuf {
    home.join("Library/Application Support/Quark/desktop.sock")
}

/// Takes `quark://open/...` links, one per line, on `path` and hands each to `open`.
/// Replaces a socket left by an earlier run; gives up quietly when it cannot listen.
#[cfg(unix)]
fn serve_links(
    path: std::path::PathBuf,
    open: impl Fn(&str) + Send + 'static,
) -> std::io::Result<()> {
    use std::io::BufRead;
    use std::os::unix::net::UnixListener;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            for line in std::io::BufReader::new(stream)
                .lines()
                .map_while(Result::ok)
            {
                open(line.trim());
            }
        }
    });
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_deep_link::init())
        .setup(|app| {
            use tauri_plugin_deep_link::DeepLinkExt;
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    open_link(&handle, url.as_str());
                }
            });
            #[cfg(unix)]
            if let Ok(home) = app.path().home_dir() {
                let handle = app.handle().clone();
                let _ = serve_links(link_socket(&home), move |url| open_link(&handle, url));
            }
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
            // Launched by a link: open its route once the window exists.
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                for url in urls {
                    open_link(app.handle(), url.as_str());
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Quark desktop app");
}

#[cfg(test)]
mod tests {
    use super::{encode, link_hash, page_path};

    #[cfg(unix)]
    #[test]
    fn takes_links_on_the_socket() {
        use std::io::Write;
        use std::sync::mpsc;
        let dir = std::env::temp_dir().join(format!("quark-links-{}", std::process::id()));
        let path = super::link_socket(&dir);
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        super::serve_links(path.clone(), move |url| {
            let _ = tx.lock().unwrap().send(url.to_string());
        })
        .unwrap();
        let mut s = std::os::unix::net::UnixStream::connect(&path).unwrap();
        s.write_all(b"quark://open/t/tsk_1\nquark://open/hosts\n")
            .unwrap();
        drop(s);
        let wait = std::time::Duration::from_secs(5);
        assert_eq!(rx.recv_timeout(wait).unwrap(), "quark://open/t/tsk_1");
        assert_eq!(rx.recv_timeout(wait).unwrap(), "quark://open/hosts");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn links_name_hash_routes() {
        assert_eq!(
            link_hash("quark://open/t/tsk_1").as_deref(),
            Some("#/t/tsk_1")
        );
        assert_eq!(
            link_hash("quark://open/p/prj_1/work").as_deref(),
            Some("#/p/prj_1/work")
        );
        assert_eq!(link_hash("quark://open/").as_deref(), Some("#/"));
        assert_eq!(link_hash("quark://open").as_deref(), Some("#/"));
        assert_eq!(
            link_hash("quark://open/inbox/a%2Fb?x=1").as_deref(),
            Some("#/inbox/a%2Fb")
        );
        assert_eq!(link_hash("quark://open/t/x';alert(1)//"), None);
        assert_eq!(link_hash("https://example.com"), None);
    }

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
