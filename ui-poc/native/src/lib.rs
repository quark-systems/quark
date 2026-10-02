//! Web build (wasm32 only): the same app as the desktop binary, started from the browser.
//! The daemon URL comes from the page's `?daemon=` query parameter.
#![cfg(target_family = "wasm")]

use std::borrow::Cow;

use anyhow::{Result, anyhow};
use wasm_bindgen::prelude::*;
use warpui::{AssetProvider, platform};
use web_time::Instant;

mod api;
mod app;
mod diff;
mod elements;
mod markdown;
mod perf;
mod term;

struct NoAssets;
impl AssetProvider for NoAssets {
    fn get(&self, path: &str) -> Result<Cow<'_, [u8]>> {
        Err(anyhow!("no asset at {path}"))
    }
}

fn daemon_from_query() -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    web_sys::UrlSearchParams::new_with_str(&search).ok()?.get("daemon")
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    let base = daemon_from_query().unwrap_or_else(|| "http://127.0.0.1:7420".into());
    log::info!("quark web: daemon {base}");
    let app = platform::AppBuilder::new(platform::AppCallbacks::default(), Box::new(NoAssets), None);
    let _ = app.run(move |ctx| {
        app::register_bindings(ctx);
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
        let opts = warpui::AddWindowOptions { title: Some("Quark (web POC)".into()), ..Default::default() };
        ctx.add_window(opts, move |ctx| {
            app::RootView::new(ctx, app::Options { base, bench: None, term: "none".into() })
        });
    });
}
