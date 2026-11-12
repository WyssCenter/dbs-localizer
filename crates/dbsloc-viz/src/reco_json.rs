// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Reco v2 JSON parsing.
//!
//! JSON schema (specs/05 §8):
//! ```json
//! { "version": 2, "subj_id": "…", "method": "…",
//!   "mni": { "1": { "trajectory": [[x,y,z],…], "coords_mm": [[x,y,z],…] },
//!            "2": { "markers": { "head": [x,y,z], "tail": [x,y,z] } } } }
//! ```
//! `trajectory` is preferred; `markers` head/tail is the fallback.
//!
//! Sides are requested by **hemisphere**, never by raw key, because the key order changed:
//! files with `meta.side_keys = "1=right,2=left"` are read by key (`specs/05` §8); older files,
//! written with `"1"` = left and no `meta.side_keys`, are read by mean *x* (two sides: the larger
//! is right; one side: its sign). This is the rule `dbsloc.reconstruction.io.side_hemispheres`
//! applies in Python.

use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

use serde::Deserialize;

use crate::error::{VizError, VizResult};

/// Rigid-frame markers in MNI mm (`specs/05` §8). `x` / `y` are the ring-plane fiducials the
/// reconstruction writes for directional-lead orientation; older files may omit them.
/// `x` / `y` are read only by the `scene-viewer` lead mesh, so `head` / `tail` cover default builds.
#[derive(Debug, Clone, Deserialize)]
pub struct MniMarkers {
    pub head: [f64; 3],
    pub tail: [f64; 3],
    #[serde(default)]
    pub x: Option<[f64; 3]>,
    #[serde(default)]
    pub y: Option<[f64; 3]>,
}

#[derive(Debug, Deserialize)]
struct MniSide {
    trajectory: Option<Vec<[f64; 3]>>,
    coords_mm: Option<Vec<[f64; 3]>>,
    markers: Option<MniMarkers>,
}

#[derive(Debug, Deserialize)]
struct RecoV2Raw {
    version: i64,
    mni: HashMap<String, MniSide>,
    #[serde(default)]
    reference: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    meta: Option<serde_json::Value>,
}

/// The side-key convention [`hemisphere_key`] trusts when a file declares it.
pub const SIDE_KEYS_CONVENTION: &str = "1=right,2=left";

/// Which lead of a bilateral reconstruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hemisphere {
    Right,
    Left,
}

impl Hemisphere {
    /// `"1"` / `"r"` / `"right"` and `"2"` / `"l"` / `"left"`, case-insensitive; the numbers
    /// follow the current Reco v2 order (`"1"` = right).
    pub fn parse(s: &str) -> VizResult<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "1" | "r" | "right" => Ok(Hemisphere::Right),
            "2" | "l" | "left" => Ok(Hemisphere::Left),
            other => Err(VizError::Json(format!(
                "side {other:?}: expected 1/right or 2/left"
            ))),
        }
    }

    fn key(self) -> &'static str {
        match self {
            Hemisphere::Right => "1",
            Hemisphere::Left => "2",
        }
    }
}

fn mean_x(s: &MniSide) -> Option<f64> {
    if let Some(m) = &s.markers {
        return Some(0.5 * (m.head[0] + m.tail[0]));
    }
    let pts = s
        .trajectory
        .as_ref()
        .filter(|t| !t.is_empty())
        .or(s.coords_mm.as_ref().filter(|c| !c.is_empty()))?;
    Some(pts.iter().map(|p| p[0]).sum::<f64>() / pts.len() as f64)
}

fn hemisphere_key(raw: &RecoV2Raw, hemi: Hemisphere) -> VizResult<Option<String>> {
    let declared = raw
        .meta
        .as_ref()
        .and_then(|m| m.get("side_keys"))
        .and_then(|v| v.as_str());
    match declared {
        Some(SIDE_KEYS_CONVENTION) => {
            return Ok(raw
                .mni
                .contains_key(hemi.key())
                .then(|| hemi.key().to_owned()))
        }
        Some(other) => {
            return Err(VizError::Json(format!(
                "meta.side_keys {other:?} is not a known convention"
            )))
        }
        None => {}
    }
    let mut xs: Vec<(String, f64)> = raw
        .mni
        .iter()
        .filter_map(|(k, s)| mean_x(s).map(|x| (k.clone(), x)))
        .collect();
    match xs.len() {
        1 => {
            let (k, x) = xs.remove(0);
            let here = if x > 0.0 {
                Hemisphere::Right
            } else {
                Hemisphere::Left
            };
            Ok((here == hemi).then_some(k))
        }
        2 => {
            xs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            if (xs[0].1 - xs[1].1).abs() < 1e-9 {
                return Err(VizError::Json(
                    "both sides have the same mean x; cannot tell the hemispheres apart".into(),
                ));
            }
            let i = if hemi == Hemisphere::Right { 0 } else { 1 };
            Ok(Some(xs[i].0.clone()))
        }
        n => Err(VizError::Json(format!(
            "expected one or two sides with coordinates in the mni block, found {n}"
        ))),
    }
}

/// Trajectory + optional contact rows for one hemisphere (MNI mm).
#[derive(Debug, Clone)]
pub struct MniSideGeometry {
    /// Read by the desktop viewers; the browser viewer only needs the parsed side geometry from
    /// [`load_mni_side_geometry_from_str`] and does not read the trajectory field directly.
    pub trajectory: Vec<[f64; 3]>,
    /// Populated from `coords_mm` in JSON; drawn when feature `scene-viewer` is enabled.
    pub contacts: Option<Vec<[f64; 3]>>,
    /// Rigid-frame markers (`head` / `tail` / `x` / `y`) when the file carries them; these place
    /// the procedural lead mesh in world mm (`scene-viewer`).
    pub markers: Option<MniMarkers>,
    /// `meta.elmodel` — catalogue name used to pick the electrode geometry (`scene-viewer`).
    pub elmodel: Option<String>,
}

/// Trajectory and contacts of one hemisphere (`side` as in [`load_mni_side_geometry_from_str`]).
#[cfg(not(target_arch = "wasm32"))]
pub fn load_mni_side_geometry(path: &Path, side: &str) -> VizResult<MniSideGeometry> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| VizError::Json(format!("cannot read {}: {e}", path.display())))?;
    load_mni_side_geometry_from_str(&text, side)
        .map_err(|e| VizError::Json(format!("{}: {e}", path.display())))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load_mni_side_geometry_opt(path: &Path, side: &str) -> VizResult<Option<MniSideGeometry>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| VizError::Json(format!("cannot read {}: {e}", path.display())))?;
    load_mni_side_geometry_from_str_opt(&text, side)
        .map_err(|e| VizError::Json(format!("{}: {e}", path.display())))
}

/// Byte-based variant of [`load_mni_side_geometry`] for the browser viewer
/// (`docs/SERVER_CLIENT_PLAN.md` §6.1): parse Reco v2 JSON already in memory.
pub fn load_mni_side_geometry_from_bytes(bytes: &[u8], side: &str) -> VizResult<MniSideGeometry> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| VizError::Json(format!("reco JSON is not UTF-8: {e}")))?;
    load_mni_side_geometry_from_str(text, side)
}

/// Parse a Reco v2 JSON **string** (no filesystem) for one hemisphere. `side` is parsed by
/// [`Hemisphere::parse`] and resolved to a key as the module docs describe.
///
/// Priority: `trajectory` array, then `markers.head`/`tail` as two-point fallback.
/// Returns at least 2 points or an error.
pub fn load_mni_side_geometry_from_str(text: &str, side: &str) -> VizResult<MniSideGeometry> {
    load_mni_side_geometry_from_str_opt(text, side)?.ok_or_else(|| {
        VizError::Json(format!(
            "side {side:?}: this reconstruction has no lead on that hemisphere"
        ))
    })
}

pub fn load_mni_side_geometry_from_str_opt(
    text: &str,
    side: &str,
) -> VizResult<Option<MniSideGeometry>> {
    load_block_side_geometry_from_str_opt(text, side, "mni")
}

/// Like [`load_mni_side_geometry_opt`] for the `mni` block or the `reference` block
/// (`specs/05` §8) of a file.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_block_side_geometry_opt(
    path: &Path,
    side: &str,
    block: &str,
) -> VizResult<Option<MniSideGeometry>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| VizError::Json(format!("cannot read {}: {e}", path.display())))?;
    load_block_side_geometry_from_str_opt(&text, side, block)
        .map_err(|e| VizError::Json(format!("{}: {e}", path.display())))
}

/// Parse one hemisphere of the `mni` or the `reference` block of a Reco v2 JSON string.
pub fn load_block_side_geometry_from_str_opt(
    text: &str,
    side: &str,
    block: &str,
) -> VizResult<Option<MniSideGeometry>> {
    let hemi = Hemisphere::parse(side)?;
    let mut raw: RecoV2Raw =
        serde_json::from_str(text).map_err(|e| VizError::Json(e.to_string()))?;
    if block == "reference" {
        let reference = raw
            .reference
            .take()
            .ok_or_else(|| VizError::Json("the file has no reference block".to_owned()))?;
        raw.mni = reference
            .into_iter()
            .filter_map(|(k, v)| serde_json::from_value::<MniSide>(v).ok().map(|s| (k, s)))
            .collect();
    } else if block != "mni" {
        return Err(VizError::Json(format!("unknown block {block:?}")));
    }
    if raw.version != 2 {
        return Err(VizError::Json(format!(
            "expected version=2, got {}",
            raw.version
        )));
    }
    let Some(key) = hemisphere_key(&raw, hemi)? else {
        return Ok(None);
    };
    let s = raw
        .mni
        .get(&key)
        .ok_or_else(|| VizError::Json(format!("side {key:?} ({hemi:?}) not in mni block")))?;

    let traj = if let Some(tr) = &s.trajectory {
        if tr.len() < 2 {
            return Err(VizError::Json(format!(
                "trajectory side {side:?}: need ≥2 points, got {}",
                tr.len()
            )));
        }
        tr.clone()
    } else {
        let mk = s.markers.as_ref().ok_or_else(|| {
            VizError::Json(format!("side {side:?}: no trajectory and no markers"))
        })?;
        vec![mk.head, mk.tail]
    };

    let elmodel = raw
        .meta
        .as_ref()
        .and_then(|m| m.get("elmodel"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    Ok(Some(MniSideGeometry {
        trajectory: traj,
        contacts: s.coords_mm.clone(),
        markers: s.markers.clone(),
        elmodel,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    fn side(x: f64) -> String {
        format!(r#"{{ "markers": {{ "head": [{x}, -10.0, -8.0], "tail": [{x}, -6.0, -1.0] }} }}"#)
    }

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "dbsloc_reco_json_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|t| t.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn declared_convention_is_read_by_key() {
        let d = dir();
        let p = write(
            &d,
            "new.json",
            &format!(
                r#"{{ "version": 2, "meta": {{ "side_keys": "1=right,2=left" }},
                     "mni": {{ "1": {r}, "2": {l} }} }}"#,
                r = side(12.0),
                l = side(-12.0)
            ),
        );
        assert!(load_mni_side_geometry(&p, "right").unwrap().trajectory[0][0] > 0.0);
        assert!(load_mni_side_geometry(&p, "1").unwrap().trajectory[0][0] > 0.0);
        assert!(load_mni_side_geometry(&p, "left").unwrap().trajectory[0][0] < 0.0);
    }

    #[test]
    fn pre_flip_files_are_read_by_x() {
        let d = dir();
        let p = write(
            &d,
            "old.json",
            &format!(
                r#"{{ "version": 2, "mni": {{ "1": {l}, "2": {r} }} }}"#,
                l = side(-12.0),
                r = side(12.0)
            ),
        );
        assert!(load_mni_side_geometry(&p, "1").unwrap().trajectory[0][0] > 0.0);
        assert!(load_mni_side_geometry(&p, "left").unwrap().trajectory[0][0] < 0.0);
    }

    #[test]
    fn unknown_convention_and_bad_side_are_errors() {
        let d = dir();
        let p = write(
            &d,
            "odd.json",
            &format!(
                r#"{{ "version": 2, "meta": {{ "side_keys": "1=left" }}, "mni": {{ "1": {r} }} }}"#,
                r = side(12.0)
            ),
        );
        assert!(load_mni_side_geometry(&p, "right").is_err());
        assert!(Hemisphere::parse("3").is_err());
    }

    #[test]
    fn a_unilateral_reconstruction_has_no_other_side() {
        let body = format!(
            r#"{{ "version": 2, "meta": {{ "side_keys": "1=right,2=left", "sides_found": ["left"] }},
                 "mni": {{ "2": {l} }} }}"#,
            l = side(-12.0)
        );
        assert!(load_mni_side_geometry_from_str_opt(&body, "right")
            .unwrap()
            .is_none());
        assert!(load_mni_side_geometry_from_str_opt(&body, "left")
            .unwrap()
            .is_some());
        assert!(load_mni_side_geometry_from_str(&body, "right").is_err());
        let legacy = format!(
            r#"{{ "version": 2, "mni": {{ "1": {r} }} }}"#,
            r = side(12.0)
        );
        assert!(load_mni_side_geometry_from_str_opt(&legacy, "left")
            .unwrap()
            .is_none());
        assert!(load_mni_side_geometry_from_str_opt(&legacy, "right")
            .unwrap()
            .is_some());
    }

    #[test]
    fn byte_and_str_loaders_match_path_loader() {
        let body = format!(
            r#"{{ "version": 2, "meta": {{ "side_keys": "1=right,2=left" }},
                 "mni": {{ "1": {r}, "2": {l} }} }}"#,
            r = side(12.0),
            l = side(-12.0)
        );
        let from_str = load_mni_side_geometry_from_str(&body, "left").expect("from_str");
        let from_bytes =
            load_mni_side_geometry_from_bytes(body.as_bytes(), "left").expect("from_bytes");
        assert_eq!(from_str.trajectory, from_bytes.trajectory);
        assert!(from_bytes.trajectory[0][0] < 0.0, "left side parsed by key");
        assert!(load_mni_side_geometry_from_str(&body, "3").is_err());
    }
}
