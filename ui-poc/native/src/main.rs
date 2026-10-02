//! Quark desktop UI POC — native Rust on Warp's warpui framework.

use std::borrow::Cow;
use web_time::Instant;

use anyhow::{anyhow, Result};
use warpui::geometry::vector::vec2f;
use warpui::{platform, AssetProvider};

mod api;
mod app;
mod diff;
mod elements;
mod markdown;
mod perf;
mod term;
mod term_alacritty;
#[cfg(feature = "ghostty")]
mod term_ghostty;

struct NoAssets;
impl AssetProvider for NoAssets {
    fn get(&self, path: &str) -> Result<Cow<'_, [u8]>> {
        Err(anyhow!("no asset at {path}"))
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut base = std::env::var("QUARK_DAEMON").unwrap_or_else(|_| "http://127.0.0.1:7420".into());
    let mut bench = None;
    let mut term = std::env::var("QUARK_TERM").unwrap_or_else(|_| "alacritty".into());
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--bench" => {
                let secs = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(20);
                i += 1;
                bench = Some(app::BenchConfig { seconds: secs, use_xdotool: false });
            }
            "--xdotool" => {
                if let Some(b) = bench.as_mut() {
                    b.use_xdotool = true;
                }
            }
            "--term" => {
                term = args.get(i + 1).cloned().unwrap_or(term);
                i += 1;
            }
            "--daemon" => {
                base = args.get(i + 1).cloned().unwrap_or(base);
                i += 1;
            }
            "-h" | "--help" => {
                println!("quark-ui-native-poc [--daemon URL] [--term alacritty|ghostty] [--bench SECONDS [--xdotool]]");
                return Ok(());
            }
            _ => {}
        }
        i += 1;
    }
    if args.iter().any(|a| a == "--xdotool")
        && let Some(b) = bench.as_mut()
    {
        b.use_xdotool = true;
    }

    let app = platform::AppBuilder::new(platform::AppCallbacks::default(), Box::new(NoAssets), None);
    if term == "ghostty" && !cfg!(feature = "ghostty") {
        eprintln!("built without the `ghostty` feature; using alacritty_terminal");
        term = "alacritty".into();
    }
    let _ = app.run(move |ctx| {
        app::register_bindings(ctx);
        // Frame timing: layout start (FrameProbe) -> frame presented (on_frame_drawn).
        ctx.on_frame_drawn(|_, _| {
            let now = Instant::now();
            if let Some(start) = elements::LAYOUT_START.with(|s| s.take()) {
                let paint_end = elements::PAINT_END.with(|s| s.take()).unwrap_or(now);
                app::PERF.with(|p| {
                    let mut p = p.borrow_mut();
                    p.frame(now, now.duration_since(start));
                    p.split(paint_end.duration_since(start), now.duration_since(paint_end));
                });
            }
        });
        let opts = warpui::AddWindowOptions {
            title: Some("Quark (native POC)".into()),
            window_bounds: warpui::platform::WindowBounds::ExactSize(vec2f(1560., 960.)),
            ..Default::default()
        };
        ctx.add_window(opts, move |ctx| app::RootView::new(ctx, app::Options { base, bench, term }));
    });
    Ok(())
}
