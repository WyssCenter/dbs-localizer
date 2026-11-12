// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Precomputed AHEAD atlas meshes for the browser viewer.
//!
//! `docs/SERVER_CLIENT_PLAN.md` §6.3 and decision **D7** (2026-10-10): the ten structure meshes
//! under `resources/atlases/ahead/structures_mni09b/precomputed_obj/` are **committed** as
//! Wavefront `.obj` files (4 MB). The web viewer fetches these bytes and parses them with
//! [`TriMeshScene::parse_str`] — it **never** runs marching cubes (`atlas_marching`) on `wasm32`,
//! and it never reads the `.cache.json` sidecars.
//!
//! The native viewer meshes from the mask NIfTIs through `atlas_marching`, whose cache key is the
//! mask's content hash plus the meshing parameters (WP5, 2026-10-15), so the committed `.obj`
//! files and their sidecars are accepted on every checkout and in a read-only install; recomputed
//! meshes go to the per-user cache directory. This module is the byte-based path the web build
//! uses.

#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

use crate::atlas_wavefront::TriMeshScene;
use crate::error::{VizError, VizResult};

/// Stem names of the ten committed AHEAD MNI09b structure meshes (extension `.obj`), matching the
/// mask NIfTI names in `resources/atlases/ahead/structures_mni09b/`.
pub const AHEAD_PRECOMPUTED_OBJECTS: [&str; 10] = [
    "atlas-gpe_hem-l_mask-105_mni09b",
    "atlas-gpe_hem-r_mask-105_mni09b",
    "atlas-gpi_hem-l_mask-105_mni09b",
    "atlas-gpi_hem-r_mask-105_mni09b",
    "atlas-rn_hem-l_mask-105_mni09b",
    "atlas-rn_hem-r_mask-105_mni09b",
    "atlas-sn_hem-l_mask-105_mni09b",
    "atlas-sn_hem-r_mask-105_mni09b",
    "atlas-stn_hem-l_mask-104_mni09b",
    "atlas-stn_hem-r_mask-104_mni09b",
];

/// One parsed structure mesh with its catalogue stem name.
#[derive(Debug, Clone)]
pub struct PrecomputedAtlasMesh {
    pub name: String,
    pub mesh: TriMeshScene,
}

/// The file name (`<stem>.obj`) the web client resolves against `/files/{key}`.
pub fn ahead_object_file_name(stem: &str) -> String {
    format!("{stem}.obj")
}

/// Parse precomputed `.obj` meshes already in memory — the browser viewer path. `files` are
/// `(name, bytes)` pairs, e.g. fetched from the server's file endpoint.
pub fn load_precomputed_ahead_from_bytes(
    files: &[(&str, &[u8])],
) -> VizResult<Vec<PrecomputedAtlasMesh>> {
    files
        .iter()
        .map(|(name, bytes)| {
            let text = std::str::from_utf8(bytes)
                .map_err(|e| VizError::NiftiLoad(format!("obj {name}: not UTF-8: {e}")))?;
            let mesh = TriMeshScene::parse_str(text)
                .map_err(|e| VizError::NiftiLoad(format!("obj {name}: {e}")))?;
            Ok(PrecomputedAtlasMesh {
                name: (*name).to_owned(),
                mesh,
            })
        })
        .collect()
}

/// Read and parse the ten committed `.obj` meshes from a directory. Desktop convenience only —
/// the browser build has no filesystem and uses [`load_precomputed_ahead_from_bytes`].
#[cfg(not(target_arch = "wasm32"))]
pub fn load_precomputed_ahead_from_dir(dir: &Path) -> VizResult<Vec<PrecomputedAtlasMesh>> {
    let mut out = Vec::with_capacity(AHEAD_PRECOMPUTED_OBJECTS.len());
    for stem in AHEAD_PRECOMPUTED_OBJECTS {
        let path = dir.join(ahead_object_file_name(stem));
        let text = std::fs::read_to_string(&path)
            .map_err(|e| VizError::NiftiLoad(format!("obj {}: {e}", path.display())))?;
        let mesh = TriMeshScene::parse_str(&text)
            .map_err(|e| VizError::NiftiLoad(format!("obj {}: {e}", path.display())))?;
        out.push(PrecomputedAtlasMesh {
            name: stem.to_owned(),
            mesh,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_committed_meshes_from_bytes() {
        let obj: &[u8] = b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
        let files: Vec<(&str, &[u8])> = AHEAD_PRECOMPUTED_OBJECTS
            .iter()
            .map(|s| (*s, obj))
            .collect();
        let meshes = load_precomputed_ahead_from_bytes(&files).expect("from_bytes");
        assert_eq!(meshes.len(), 10);
        assert!(meshes.iter().all(|m| m.mesh.indices.len() == 3));
        assert_eq!(meshes[0].name, "atlas-gpe_hem-l_mask-105_mni09b");
    }

    /// Reads the committed assets when present (repo checkout); skipped otherwise.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn committed_precomputed_dir_parses_when_present() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../resources/atlases/ahead/structures_mni09b/precomputed_obj");
        if !dir.is_dir() {
            return;
        }
        let meshes = load_precomputed_ahead_from_dir(&dir).expect("committed meshes parse");
        assert_eq!(meshes.len(), 10);
        for m in &meshes {
            assert!(!m.mesh.positions.is_empty(), "{} has no vertices", m.name);
            assert!(m.mesh.indices.len() >= 3, "{} has no triangles", m.name);
        }
    }
}
