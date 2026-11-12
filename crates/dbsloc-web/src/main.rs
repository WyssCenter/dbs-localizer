// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Entry point for the `dbsloc-web` browser client.
//!
//! The crate is wasm-only: on `wasm32` this `main` starts the eframe `WebRunner` against the
//! `#dbsloc_canvas` element. On any other target it is a stub that points at `trunk`, so the
//! crate is never silently broken natively. See `docs/SERVER_CLIENT_PLAN.md` §5.

#[cfg(target_arch = "wasm32")]
fn main() {
    dbsloc_web::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    println!(
        "dbsloc-web is a wasm32 browser client; build it with `trunk build` (wasm32-unknown-unknown)."
    );
}
