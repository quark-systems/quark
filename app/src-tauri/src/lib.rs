// The webview talks to quarkd directly over HTTP and WebSocket, so the Rust side only
// opens the window. Two environment variables help automation and development:
// - QUARK_DAEMON points the app at a daemon other than http://127.0.0.1:7380.
// - QUARK_QUERY is appended to the page URL as-is (e.g. "renderer=webgl").
use tauri::{WebviewUrl, WebviewWindowBuilder};

fn page_path() -> String {
    let mut query: Vec<String> = Vec::new();
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
        .setup(|app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App(page_path().into()))
                .title("Quark")
                .inner_size(1440.0, 900.0)
                .min_inner_size(960.0, 600.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Quark desktop app");
}

#[cfg(test)]
mod tests {
    use super::encode;

    #[test]
    fn encodes_daemon_urls() {
        assert_eq!(
            encode("http://127.0.0.1:7380"),
            "http%3A%2F%2F127.0.0.1%3A7380"
        );
    }
}
