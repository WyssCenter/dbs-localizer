// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Readers for the connectome formats of `specs/15` §3 and the fibre spatial index.

mod error;
pub mod fibres;
pub mod functional;
pub mod index;
pub mod kind;
mod matio;
pub mod multitract;
pub mod trk;

pub use error::{Error, Result};
pub use fibres::{
    matlab_dims_from_hdf5, probe_point_count, read_data_mat, read_data_mat_cancellable,
    read_fibres, read_fibres_cancellable, write_data_mat, FibreSet, FibreSpace,
};
pub use functional::{FunctionalConnectome, RunRef, RunTimecourses, Subset, VolumeGrid};
pub use index::{FibreIndex, MaskView};
pub use kind::{detect_kind, ConnectomeKind};
pub use multitract::{read_multitract, MultiTract, Pathway};
pub use trk::{read_trk, read_trk_bytes};

/// A 4 x 4 row-major affine.
pub type Affine = [[f64; 4]; 4];

/// Version of the on-disk cache layout of `specs/15` §3.5 (the cache itself is deferred).
pub const CACHE_VERSION: u32 = 1;

/// Type of the on-disk cache metadata of `specs/15` §3.5 (the cache is not built yet).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CacheMeta {
    pub source_path: String,
    pub source_size: u64,
    pub source_mtime: i64,
    pub source_probe_sha256: String,
    pub format: String,
    pub p: u64,
    pub k: u64,
    pub bbox_mm: [[f64; 3]; 2],
    pub space: String,
    pub built_by: String,
    pub cache_version: u32,
}

/// The cache key of `specs/15` §3.5.
pub fn cache_key(class: &str, folder_name: &str, path_sha256_hex: &str) -> String {
    let name: String = folder_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let short: String = path_sha256_hex.chars().take(8).collect();
    format!("{class}-{name}-{short}")
}
