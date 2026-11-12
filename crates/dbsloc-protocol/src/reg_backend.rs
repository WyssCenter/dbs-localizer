// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Registration execution backend selection (`specs/04` §5.4).
//!
//! Only the [`RegBackend`] enum and its pure methods live here, so the type is available to the
//! client Cargo.toml and browser without the GPU. Everything that actually touches a device
//! (`GpuProbe`, the probe, the resolution rule and the engines) stays in
//! `dbsloc-pipeline/src/reg_backend.rs`.

use serde::{Deserialize, Serialize};

/// Environment variable that selects the registration backend (the command-line flag wins).
pub const REG_BACKEND_ENV: &str = "DBSLOC_REG_BACKEND";

/// Which engine runs the registration of the `normalize` stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RegBackend {
    /// GPU when a hardware adapter passes the device self-test, ANTs otherwise.
    #[default]
    Auto,
    /// The GPU engine; an error when no usable adapter exists.
    Wgpu,
    /// The bundled ANTs processes (the CPU path).
    Ants,
}

impl RegBackend {
    /// Parses `auto`, `wgpu` or `ants` (case-insensitive, surrounding blanks ignored).
    pub fn parse(raw: &str) -> Result<RegBackend, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(RegBackend::Auto),
            "wgpu" => Ok(RegBackend::Wgpu),
            "ants" => Ok(RegBackend::Ants),
            _ => Err(format!(
                "E_REG_BACKEND: unknown registration backend {raw:?} (known: auto, wgpu, ants)"
            )),
        }
    }

    /// The spelling used in options, records and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            RegBackend::Auto => "auto",
            RegBackend::Wgpu => "wgpu",
            RegBackend::Ants => "ants",
        }
    }
}
