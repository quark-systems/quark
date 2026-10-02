// The Rust side is intentionally minimal: the webview talks to the daemon directly
// over HTTP/WebSocket. Two tiny additions:
// - QUARK_QUERY env var is appended to the page URL (e.g. "bench=1&exit=1&daemon=http://127.0.0.1:7431"),
//   so automation can drive the same query flags as the web build.
// - bench_report prints the bench JSON to process stdout so it can be scraped.
use tauri::{WebviewUrl, WebviewWindowBuilder};

#[tauri::command]
fn bench_report(json: String, exit: bool, app: tauri::AppHandle) {
    println!("QUARK_BENCH_RESULT {json}");
    if exit {
        app.exit(0);
    }
}

/// Per-phase progress lines, so a run that never finishes still leaves partial numbers.
#[tauri::command]
fn bench_progress(json: String) {
    println!("QUARK_BENCH_PROGRESS {json}");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![bench_report, bench_progress])
        .setup(|app| {
            let q = std::env::var("QUARK_QUERY").unwrap_or_default();
            let path = if q.is_empty() { "index.html".to_string() } else { format!("index.html?{q}") };
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App(path.into()))
                .title("Quark")
                .inner_size(1600.0, 1000.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
