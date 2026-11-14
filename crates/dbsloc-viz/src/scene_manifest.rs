// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Scene viewer manifest (`specs/08` §7 companion): paths for interactive 3D QC.
//!
//! Version 1. Forward-compatible: unknown fields ignored by serde where using Value, here strict struct.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One Lead-DBS-style atlas tree (`lh/`, `rh/`, … per-structure NIfTIs, `specs/11` §2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasLeadSetV1 {
    pub name: String,
    pub dir: PathBuf,
    /// Stable identifier persisted as the viewer's chosen atlas; `None` → `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `None` → `lead` when any of `lh/`, `rh/`, `mixed/`, `midline/` exists, else `flat`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<AtlasLayoutV1>,
    /// Structures whose name contains any of these (case-insensitive) start visible. Used only
    /// when the set has no `index`. `None` → all on (Lead layout) or the global fallback.
    #[serde(default)]
    pub default_on_substrings: Option<Vec<String>>,
    /// Parsed `atlas_index.mat` (`specs/11` §3) as emitted by the pipeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<AtlasIndexV1>,
}

impl AtlasLeadSetV1 {
    pub fn set_id(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AtlasLayoutV1 {
    Lead,
    Flat,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AtlasIndexV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation_short: Option<String>,
    #[serde(default)]
    pub citation_long: Vec<String>,
    #[serde(default)]
    pub structures: Vec<AtlasStructureV1>,
    #[serde(default)]
    pub presets: Vec<AtlasPresetV1>,
    /// 0-based index into `presets` selecting the initially visible structures.
    #[serde(default, deserialize_with = "de_opt_index")]
    pub default_preset: Option<usize>,
    #[serde(default)]
    pub labels: Vec<AtlasLabelSetV1>,
    #[serde(default)]
    pub skipped: Vec<AtlasSkippedV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasStructureV1 {
    /// 0-based position in the atlas `names` list; preset and label indices refer to it.
    /// `None` → the position in `structures`.
    #[serde(
        default,
        deserialize_with = "de_opt_index",
        skip_serializing_if = "Option::is_none"
    )]
    pub index: Option<usize>,
    pub file: String,
    pub name: String,
    /// Atlas type code, `specs/11` §2.1.
    #[serde(deserialize_with = "de_index")]
    pub kind: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tissue_type: Option<f64>,
    #[serde(default)]
    pub sides: Vec<AtlasStructureSideV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AtlasSideV1 {
    Right,
    Left,
    Midline,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasStructureSideV1 {
    pub side: AtlasSideV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<f32>,
    /// Iso-level in processed-image units (`specs/11` §2.5, §4.1 step 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasPresetV1 {
    pub label: String,
    /// 0-based structure indices.
    #[serde(default, deserialize_with = "de_index_vec")]
    pub show: Vec<usize>,
    #[serde(default, deserialize_with = "de_index_vec")]
    pub hide: Vec<usize>,
    /// `absolute` hides structures in neither list; anything else (`relative`, absent) leaves
    /// them unchanged.
    #[serde(default)]
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasLabelSetV1 {
    pub name: String,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasSkippedV1 {
    pub file: String,
    #[serde(default)]
    pub kind_text: String,
}

fn index_from_f64<E: serde::de::Error>(v: f64) -> Result<usize, E> {
    if v.is_finite() && v >= 0.0 && v.fract() == 0.0 && v <= u32::MAX as f64 {
        Ok(v as usize)
    } else {
        Err(E::custom(format!(
            "expected a non-negative integer, got {v}"
        )))
    }
}

fn de_index<'de, D: serde::Deserializer<'de>>(d: D) -> Result<usize, D::Error> {
    index_from_f64(f64::deserialize(d)?)
}

fn de_opt_index<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<usize>, D::Error> {
    Option::<f64>::deserialize(d)?
        .map(index_from_f64)
        .transpose()
}

fn de_index_vec<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<usize>, D::Error> {
    Option::<Vec<f64>>::deserialize(d)?
        .unwrap_or_default()
        .into_iter()
        .map(index_from_f64)
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneBackdropV1 {
    pub id: String,
    pub label: String,
    pub path: PathBuf,
    #[serde(default)]
    pub kind: String,
}

/// One written binary volume of tissue activated, in the scene's space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneVtaV1 {
    /// `right` or `left`.
    pub side: String,
    pub label: String,
    pub path: PathBuf,
}

/// A value for the right (`"1"`) and left (`"2"`) side of a lead pair.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SidePairV2<T> {
    #[serde(rename = "1", default)]
    pub right: T,
    #[serde(rename = "2", default)]
    pub left: T,
}

impl<T> SidePairV2<T> {
    /// The value of side index 0 (right) or 1 (left).
    pub fn get(&self, side: usize) -> &T {
        if side == 0 {
            &self.right
        } else {
            &self.left
        }
    }
}

/// One patient (or mirrored copy) of a group scene (`specs/14` §5.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupSubjectV2 {
    pub subject: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub group: u32,
    pub reco_json: PathBuf,
    /// `mni` or `reference`: the Reco v2 block in the group's template space.
    #[serde(default = "default_group_block")]
    pub block: String,
    #[serde(default)]
    pub elmodel: Option<String>,
    #[serde(default)]
    pub active_contacts: SidePairV2<Vec<bool>>,
    #[serde(default)]
    pub vta: SidePairV2<Option<PathBuf>>,
    #[serde(default)]
    pub vta_mesh: serde_json::Value,
    #[serde(default)]
    pub flipped: SidePairV2<Option<PathBuf>>,
    /// Per contact (or contact pair, see `value_kind`) value per side; `null` = missing.
    #[serde(default)]
    pub value: Option<SidePairV2<Vec<Option<f64>>>>,
    /// `contact` or `pair`.
    #[serde(default = "default_value_kind")]
    pub value_kind: String,
    #[serde(default)]
    pub mirrored: bool,
    #[serde(default)]
    pub source_subject: Option<String>,
}

fn default_group_block() -> String {
    "mni".to_owned()
}

fn default_value_kind() -> String {
    "contact".to_owned()
}

/// One group definition: number, label and colour.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupDefV2 {
    pub number: u32,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub color: Option<[f32; 3]>,
}

/// Colouring by a clinical variable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClinicalColourV2 {
    pub clinical: String,
    #[serde(default)]
    pub normalisation: String,
    #[serde(default)]
    pub colormap: String,
    #[serde(default)]
    pub range: Option<[f64; 2]>,
}

/// `"group"` or a clinical variable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GroupColourByV2 {
    Name(String),
    Clinical(ClinicalColourV2),
}

impl Default for GroupColourByV2 {
    fn default() -> Self {
        GroupColourByV2::Name("group".to_owned())
    }
}

/// The display toggles of a group scene.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupShowV2 {
    pub active_contacts: bool,
    pub passive_contacts: bool,
    pub highlight_active: bool,
    pub vtas: bool,
    pub point_cloud: bool,
    pub isovolume: bool,
}

impl Default for GroupShowV2 {
    fn default() -> Self {
        GroupShowV2 {
            active_contacts: true,
            passive_contacts: true,
            highlight_active: false,
            vtas: true,
            point_cloud: false,
            isovolume: false,
        }
    }
}

/// The `group` block of a version-2 manifest (`specs/14` §5.1).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GroupBlockV2 {
    #[serde(default)]
    pub guid: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subjects: Vec<GroupSubjectV2>,
    #[serde(default)]
    pub groups: Vec<GroupDefV2>,
    #[serde(default)]
    pub colour_by: GroupColourByV2,
    #[serde(default)]
    pub mirror: bool,
    #[serde(default)]
    pub show: GroupShowV2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneManifestV1 {
    #[serde(default = "default_scene_manifest_version")]
    pub manifest_version: u32,
    pub subj_id: String,
    /// Human-readable space label (e.g. `MNI152NLin2009bAsym`).
    #[serde(default)]
    pub space: String,
    /// Template T1w (or warped anchor) NIfTI in the same mm/RAS space as `reco_json.mni`.
    pub template_nii: PathBuf,
    /// Reco v2 JSON (trajectory + `coords_mm` per side); empty in a group scene.
    #[serde(default)]
    pub reco_json: PathBuf,
    /// Optional directory of `.obj` files (one structure per file); filenames used as labels.
    #[serde(default)]
    pub atlas_obj_dir: Option<PathBuf>,
    /// Optional integer labeled NIfTI — meshed with marching cubes per label id.
    #[serde(default)]
    pub atlas_nii: Option<PathBuf>,
    /// Max labels to mesh when using `atlas_nii` (skip background 0); `None` = all positive ids (capped at 256 for safety).
    #[serde(default)]
    pub atlas_max_labels: Option<u32>,
    /// Multiple Lead-layout atlas roots (GUI switches between them).
    #[serde(default)]
    pub atlas_lead_sets: Vec<AtlasLeadSetV1>,
    #[serde(default)]
    pub atlas_default_id: Option<String>,
    /// Single Lead-layout root if `atlas_lead_sets` empty (backward compat).
    #[serde(default)]
    pub atlas_lead_nii_dir: Option<PathBuf>,
    /// Fallback `default_on_substrings` for Lead sets that omit the field. `OBJ` / `atlas_nii` groups ignore this.
    #[serde(default)]
    pub atlas_default_on_substrings: Option<Vec<String>>,
    /// Umbrella Laplacian smooth iterations after marching cubes (0 = off). Default mild Lead-like cleanup.
    #[serde(default = "default_atlas_mesh_smooth_iters")]
    pub atlas_mesh_smooth_iters: u32,
    /// Subtracted from continuous **source** ijk before rounding when resampling Lead masks to template.
    #[serde(default)]
    pub atlas_lead_resample_ijk_subtract: f32,
    /// When **not** resampling to template: add this to each mcubes vertex ijk before applying
    /// atlas NIfTI `sform`. Default **0**: `mcubes` vertices and the NIfTI affine are both 0-based.
    #[serde(default = "default_lead_native_mc_ijk_shift")]
    pub atlas_lead_native_mc_ijk_shift: f32,
    /// When **true**, only mesh ROIs that default-on at startup; others load when enabled in GUI.
    #[serde(default = "default_true")]
    pub atlas_lead_lazy_load: bool,
    /// When true, allow on-disk OBJ cache for Lead-layout ROIs (read/write).
    #[serde(default = "default_true")]
    pub atlas_cache_precompute: bool,
    /// Cache directory name under Lead atlas root.
    #[serde(default = "default_atlas_cache_dir_name")]
    pub atlas_cache_dir_name: String,
    /// When true, only precompute default-on ROIs during startup; others compute on enable.
    #[serde(default = "default_true")]
    pub atlas_cache_only_default_on: bool,
    /// Vertex weld epsilon (mm) before smoothing.
    #[serde(default = "default_atlas_mesh_weld_eps_mm")]
    pub atlas_mesh_weld_eps_mm: f32,
    /// Pre-blur iterations on voxel field before marching cubes (Lead-like smooth3). 0 disables.
    #[serde(default = "default_atlas_mesh_preblur_iters")]
    pub atlas_mesh_preblur_iters: u32,
    /// Morphological smoothing iterations (closing = dilate then erode) on binary field before blur/mcubes.
    /// 0 disables.
    #[serde(default)]
    pub atlas_mesh_morph_close_iters: u32,
    /// When false (default), mesh atlas ROIs at their native voxel resolution (avoids
    /// nearest-neighbour downsample to template grid which creates stripe artifacts).
    /// Set true to resample to the template grid before meshing (legacy behaviour).
    #[serde(default)]
    pub atlas_mesh_resample_to_template: bool,
    /// Debug: disable lighting, render atlas with flat color.
    #[serde(default)]
    pub atlas_unlit: bool,
    /// Debug: use flat geometric normals (derivatives) instead of mesh vertex normals.
    #[serde(default)]
    pub atlas_flat_normals: bool,
    /// Taubin smoothing iters (0 disables).
    #[serde(default = "default_atlas_mesh_taubin_iters")]
    pub atlas_mesh_taubin_iters: u32,
    #[serde(default = "default_atlas_mesh_taubin_lambda")]
    pub atlas_mesh_taubin_lambda: f32,
    #[serde(default = "default_atlas_mesh_taubin_mu")]
    pub atlas_mesh_taubin_mu: f32,
    #[serde(default)]
    pub subj_dir: Option<PathBuf>,
    #[serde(default)]
    pub backdrops: Vec<SceneBackdropV1>,
    /// Binary VTA images of the newest stimulation, drawn as surfaces (`specs/07` §5.1).
    #[serde(default)]
    pub vta: Vec<SceneVtaV1>,
    /// Version 2: the leads of a whole group (`specs/14` §5.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupBlockV2>,
}

fn default_true() -> bool {
    true
}

fn default_atlas_mesh_smooth_iters() -> u32 {
    0
}

fn default_atlas_cache_dir_name() -> String {
    "precomputed_obj".to_string()
}

fn default_atlas_mesh_weld_eps_mm() -> f32 {
    0.05
}

fn default_atlas_mesh_preblur_iters() -> u32 {
    6
}

fn default_atlas_mesh_taubin_iters() -> u32 {
    10
}

fn default_atlas_mesh_taubin_lambda() -> f32 {
    0.5
}

fn default_atlas_mesh_taubin_mu() -> f32 {
    -0.53
}

fn default_lead_native_mc_ijk_shift() -> f32 {
    0.0
}

fn default_scene_manifest_version() -> u32 {
    1
}

impl SceneManifestV1 {
    pub fn from_json_file(path: &std::path::Path) -> crate::error::VizResult<Self> {
        let canonical_manifest = std::fs::canonicalize(path).map_err(|e| {
            crate::error::VizError::Json(format!(
                "scene manifest path invalid: {}: {e}",
                path.display()
            ))
        })?;
        let manifest_dir = canonical_manifest
            .parent()
            .unwrap_or(std::path::Path::new("."));
        let text = std::fs::read_to_string(&canonical_manifest).map_err(|e| {
            crate::error::VizError::Json(format!(
                "scene manifest: {}: {e}",
                canonical_manifest.display()
            ))
        })?;
        let mut v: SceneManifestV1 = serde_json::from_str(&text).map_err(|e| {
            crate::error::VizError::Json(format!("{}: {e}", canonical_manifest.display()))
        })?;
        if !(1..=2).contains(&v.manifest_version) {
            return Err(crate::error::VizError::Json(format!(
                "scene manifest: expected manifest_version 1 or 2, got {}",
                v.manifest_version
            )));
        }
        if v.manifest_version == 2 && v.group.is_none() {
            return Err(crate::error::VizError::Json(
                "scene manifest: version 2 needs a group block".to_owned(),
            ));
        }
        let resolve = |p: &PathBuf| -> crate::error::VizResult<PathBuf> {
            let abs = if p.is_absolute() {
                p.clone()
            } else {
                manifest_dir.join(p)
            };
            std::fs::canonicalize(&abs).map_err(|e| {
                crate::error::VizError::Json(format!("manifest path {}: {e}", p.display()))
            })
        };
        v.template_nii = resolve(&v.template_nii)?;
        if !v.reco_json.as_os_str().is_empty() {
            v.reco_json = resolve(&v.reco_json)?;
        }
        if let Some(ref d) = v.atlas_obj_dir.clone() {
            v.atlas_obj_dir = Some(resolve(d)?);
        }
        if let Some(ref d) = v.atlas_nii.clone() {
            v.atlas_nii = Some(resolve(d)?);
        }
        if let Some(ref d) = v.atlas_lead_nii_dir.clone() {
            v.atlas_lead_nii_dir = Some(resolve(d)?);
        }
        for set in &mut v.atlas_lead_sets {
            set.dir = resolve(&set.dir)?;
        }
        for b in &mut v.backdrops {
            let abs = if b.path.is_absolute() {
                b.path.clone()
            } else {
                manifest_dir.join(&b.path)
            };
            b.path = std::fs::canonicalize(&abs).unwrap_or(abs);
        }
        if let Some(group) = v.group.as_mut() {
            let absolute = |p: &PathBuf| {
                if p.is_absolute() {
                    p.clone()
                } else {
                    manifest_dir.join(p)
                }
            };
            for s in &mut group.subjects {
                s.reco_json = absolute(&s.reco_json);
                for slot in [
                    &mut s.vta.right,
                    &mut s.vta.left,
                    &mut s.flipped.right,
                    &mut s.flipped.left,
                ] {
                    if let Some(p) = slot.as_ref() {
                        *slot = Some(absolute(p));
                    }
                }
            }
        }
        if let Some(ref d) = v.subj_dir.clone() {
            if d.exists() {
                v.subj_dir = Some(resolve(d)?);
            }
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_atlas_lead_sets() {
        let j = r#"{
            "manifest_version": 1,
            "subj_id": "1",
            "space": "MNI152NLin2009bAsym",
            "template_nii": "/tmp/t1.nii.gz",
            "reco_json": "/tmp/reco.json",
            "atlas_resample_lead_to_template": true,
            "atlas_lead_sets": [
                {
                    "name": "DISTAL Minimal",
                    "dir": "/tmp/distal",
                    "default_on_substrings": ["STN", "RN"]
                }
            ]
        }"#;
        let m: SceneManifestV1 = serde_json::from_str(j).unwrap();
        assert_eq!(m.atlas_lead_sets.len(), 1);
        assert_eq!(m.atlas_lead_sets[0].name, "DISTAL Minimal");
        assert_eq!(m.atlas_mesh_smooth_iters, 0);
        assert!(m.atlas_lead_lazy_load);
        assert_eq!(m.atlas_lead_native_mc_ijk_shift, 0.0);
        assert!(m.atlas_cache_precompute);
        assert_eq!(m.atlas_cache_dir_name, "precomputed_obj");
        assert!(m.atlas_cache_only_default_on);
        assert_eq!(m.atlas_mesh_weld_eps_mm, 0.05);
        assert_eq!(m.atlas_mesh_preblur_iters, 6);
        assert_eq!(m.atlas_mesh_morph_close_iters, 0);
        assert!(!m.atlas_unlit);
        assert!(!m.atlas_flat_normals);
        assert_eq!(m.atlas_mesh_taubin_iters, 10);
        assert_eq!(m.atlas_mesh_taubin_lambda, 0.5);
        assert_eq!(m.atlas_mesh_taubin_mu, -0.53);
        assert!(m.backdrops.is_empty());
        let set = &m.atlas_lead_sets[0];
        assert!(set.id.is_none() && set.layout.is_none() && set.index.is_none());
        assert_eq!(set.set_id(), "DISTAL Minimal");
    }

    #[test]
    fn atlas_index_round_trips() {
        let j = r#"{
            "subj_id": "1",
            "template_nii": "/tmp/t1.nii.gz",
            "reco_json": "/tmp/reco.json",
            "atlas_lead_sets": [{
                "name": "DISTAL Minimal (Ewert 2017)", "dir": "/tmp/distal",
                "id": "leaddbs:DISTAL Minimal (Ewert 2017)", "layout": "lead",
                "default_on_substrings": null,
                "index": {"version": 2.1, "citation_name": "DISTAL Minimal Atlas (Ewert 2017)",
                    "citation_short": "Ewert et al. 2017", "citation_long": ["a", "b"],
                    "threshold_rule": {"type": "relative_intensity", "value": 0.5},
                    "structures": [{"index": 0, "file": "STN.nii.gz", "name": "STN", "kind": 3, "tissue_type": 1,
                        "sides": [{"side": "right", "tag": "STN_right", "color": [1.0,0.5,0.0], "alpha": 0.7, "threshold": 0.4},
                                  {"side": "left", "tag": "STN_left", "color": [1.0,0.5,0.0], "alpha": 0.7, "threshold": 0.4}]}],
                    "presets": [{"label": "Primary DBS targets", "show": [0, 4.0], "hide": [], "mode": "absolute"}],
                    "default_preset": 0,
                    "labels": [{"name": "Abbreviations", "labels": ["STN"]}],
                    "skipped": [{"file": "x.mat", "kind_text": "fibers"}]}
            }]
        }"#;
        let m: SceneManifestV1 = serde_json::from_str(j).unwrap();
        let back: SceneManifestV1 =
            serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        for m in [&m, &back] {
            let set = &m.atlas_lead_sets[0];
            assert_eq!(set.set_id(), "leaddbs:DISTAL Minimal (Ewert 2017)");
            assert_eq!(set.layout, Some(AtlasLayoutV1::Lead));
            let ix = set.index.as_ref().unwrap();
            assert_eq!(ix.citation_short.as_deref(), Some("Ewert et al. 2017"));
            assert_eq!(ix.structures[0].kind, 3);
            assert_eq!(ix.structures[0].index, Some(0));
            assert_eq!(ix.structures[0].sides[1].side, AtlasSideV1::Left);
            assert_eq!(ix.structures[0].sides[0].threshold, Some(0.4));
            assert_eq!(ix.presets[0].show, vec![0, 4]);
            assert_eq!(ix.default_preset, Some(0));
            assert_eq!(ix.labels[0].labels, vec!["STN".to_string()]);
            assert_eq!(ix.skipped[0].kind_text, "fibers");
        }
        let bad = j.replace("\"kind\": 3", "\"kind\": 2.5");
        assert!(serde_json::from_str::<SceneManifestV1>(&bad).is_err());
    }

    #[test]
    fn parse_backdrops() {
        let j = r#"{
            "subj_id": "1",
            "template_nii": "/tmp/t1.nii.gz",
            "reco_json": "/tmp/reco.json",
            "backdrops": [
                {"id": "template-t1", "label": "T1w", "path": "/tmp/t1.nii.gz", "kind": "template"},
                {"id": "x", "label": "X", "path": "/tmp/x.nii.gz"}
            ]
        }"#;
        let m: SceneManifestV1 = serde_json::from_str(j).unwrap();
        assert_eq!(m.backdrops.len(), 2);
        assert_eq!(m.backdrops[1].kind, "");
    }
}
