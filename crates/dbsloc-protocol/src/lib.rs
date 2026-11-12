// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Wasm-safe shared protocol types for the Wyss DBS Localizer client and server.
//!
//! Everything here is pure data and pure logic: no filesystem, no threads, no GPU, so it compiles
//! for `wasm32-unknown-unknown` as well as native. `dbsloc-api` and `dbsloc-ui` depend on this crate
//! instead of `dbsloc-pipeline`; the pipeline re-exports the same types under their historical
//! paths (`dbsloc_pipeline::stages::Stage`, `dbsloc_pipeline::protocol::{RunEvent, StageOutcome}`,
//! `dbsloc_pipeline::reg_backend::RegBackend`, `dbsloc_pipeline::options::DEFAULT_NORM_ISOTROPIC_MM`).

#![forbid(unsafe_code)]

pub mod norm_grid;
pub mod protocol;
pub mod reg_backend;
pub mod stage;

pub use norm_grid::{NormGrid, NORM_ISOTROPIC_MM_RANGE};

/// The registration grid in millimetres used when the template's voxel size cannot be read
/// (`specs/04` §5.1 step 2); the default itself is the template's own voxel size
/// ([`NormGrid::Template`]).
pub const DEFAULT_NORM_ISOTROPIC_MM: f64 = 1.0;
