// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Wyss DBS Localizer browser client (`docs/SERVER_CLIENT_PLAN.md` §5, work package **WP4**).
//!
//! An `eframe` `WebRunner` app compiled to `wasm32-unknown-unknown`. It is the viewer half of
//! the server–client split: the GPU server runs the pipeline (`dbsloc_pipeline_runner`), this
//! client drives it over `/api/v1` (JSON + SSE) and shows the results. It shares the guided /
//! card state machines with the desktop app through [`dbsloc_ui`] and the wire types through
//! [`dbsloc_api`]; the byte loaders for the (WP5) WebGPU viewer come from `dbsloc-viz` `web`.
//!
//! This crate is a **detached workspace** (see `Cargo.toml`): it is wasm-only and is excluded
//! from the root workspace so a native `cargo build --workspace` stays green. All of the
//! modules below are `wasm32`-gated; a native build compiles only the stub `main`.

#[cfg(target_arch = "wasm32")]
mod api;
#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod files;
#[cfg(any(target_arch = "wasm32", test))]
mod logic;
#[cfg(target_arch = "wasm32")]
mod screens;
#[cfg(target_arch = "wasm32")]
mod sse;
#[cfg(any(target_arch = "wasm32", test))]
mod track;

#[cfg(target_arch = "wasm32")]
pub use app::{PersistedState, Screen, WebApp};

/// Start the browser client: install a log hook, find the canvas and hand it to eframe.
///
/// Called from `main.rs` on `wasm32`. The canvas id `dbsloc_canvas` is declared in `index.html`.
#[cfg(target_arch = "wasm32")]
pub fn start() {
    use wasm_bindgen::JsCast;

    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    wasm_bindgen_futures::spawn_local(async {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            log::error!("dbsloc-web: no browser window or document");
            return;
        };
        let Some(canvas) = document
            .get_element_by_id("dbsloc_canvas")
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        else {
            log::error!("dbsloc-web: #dbsloc_canvas is missing or is not a canvas element");
            return;
        };
        let runner = eframe::WebRunner::new();
        if let Err(err) = runner
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| Ok(Box::new(WebApp::new(cc)))),
            )
            .await
        {
            web_sys::console::error_1(&err);
        }
    });
}
