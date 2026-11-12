// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Atlas sets in the 3D viewer: structure list, colours, opacity, initial visibility and presets
//! of a Lead-DBS atlas folder (`specs/11` §2, §5).

use std::path::Path;

use crate::atlas_marching::{
    atlas_label_color, collect_flat_atlas_items, has_lead_side_folders, scan_lead_layout,
    structure_name_of, HemisphereSplit, LeadRoiSource, NamedAtlasMesh, DEFAULT_ATLAS_ALPHA,
};
use crate::error::VizResult;
use crate::scene_manifest::{
    AtlasIndexV1, AtlasLayoutV1, AtlasLeadSetV1, AtlasSideV1, AtlasStructureSideV1,
};

/// The atlas set shown when the user has not chosen one (`specs/11` §1.3).
pub const DEFAULT_ATLAS_SET_NAME: &str = "DISTAL Minimal (Ewert 2017)";

/// "BuOrR_14" stops (`specs/11` §5.1 rule 3).
const BUORR_14: [[f64; 3]; 14] = [
    [0.03, 0.353, 1.0],
    [0.2, 0.467, 1.0],
    [0.35, 0.567, 1.0],
    [0.55, 0.7, 1.0],
    [0.75, 0.833, 1.0],
    [0.9, 0.933, 1.0],
    [0.97, 0.98, 1.0],
    [1.0, 1.0, 0.8],
    [1.0, 1.0, 0.6],
    [1.0, 1.0, 0.0],
    [1.0, 0.8, 0.0],
    [1.0, 0.6, 0.0],
    [1.0, 0.4, 0.0],
    [1.0, 0.0, 0.0],
];

fn sign(v: f64) -> i8 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

fn pchip_end_slope(h0: f64, h1: f64, m0: f64, m1: f64) -> f64 {
    let d = ((2.0 * h0 + h1) * m0 - h0 * m1) / (h0 + h1);
    if sign(d) != sign(m0) {
        0.0
    } else if sign(m0) != sign(m1) && d.abs() > 3.0 * m0.abs() {
        3.0 * m0
    } else {
        d
    }
}

/// Fritsch–Carlson slopes with the MATLAB / SciPy end conditions.
fn pchip_slopes(x: &[f64], y: &[f64]) -> Vec<f64> {
    let n = x.len();
    let h: Vec<f64> = x.windows(2).map(|w| w[1] - w[0]).collect();
    let m: Vec<f64> = (0..n - 1).map(|k| (y[k + 1] - y[k]) / h[k]).collect();
    let mut d = vec![0.0; n];
    if n == 2 {
        return vec![m[0]; 2];
    }
    for k in 1..n - 1 {
        if sign(m[k - 1]) == sign(m[k]) && m[k - 1] != 0.0 && m[k] != 0.0 {
            let w1 = 2.0 * h[k] + h[k - 1];
            let w2 = h[k] + 2.0 * h[k - 1];
            d[k] = (w1 + w2) / (w1 / m[k - 1] + w2 / m[k]);
        }
    }
    d[0] = pchip_end_slope(h[0], h[1], m[0], m[1]);
    d[n - 1] = pchip_end_slope(h[n - 2], h[n - 3], m[n - 2], m[n - 3]);
    d
}

fn pchip_eval(x: &[f64], y: &[f64], d: &[f64], t: f64) -> f64 {
    let n = x.len();
    let k = x[1..n - 1].iter().take_while(|&&xk| xk <= t).count();
    let h = x[k + 1] - x[k];
    let s = (t - x[k]) / h;
    let (s2, s3) = (s * s, s * s * s);
    (2.0 * s3 - 3.0 * s2 + 1.0) * y[k]
        + (s3 - 2.0 * s2 + s) * h * d[k]
        + (-2.0 * s3 + 3.0 * s2) * y[k + 1]
        + (s3 - s2) * h * d[k + 1]
}

/// Default structure colours of an atlas without `atlas_index.mat` (`specs/11` §5.1 rule 3).
pub fn default_palette(n: usize) -> Vec<[f32; 3]> {
    if n == 0 {
        return Vec::new();
    }
    let stops = BUORR_14.len();
    let x: Vec<f64> = (0..stops).map(|i| i as f64 / (stops - 1) as f64).collect();
    let channels: Vec<(Vec<f64>, Vec<f64>)> = (0..3)
        .map(|c| {
            let y: Vec<f64> = BUORR_14.iter().map(|s| s[c]).collect();
            let d = pchip_slopes(&x, &y);
            (y, d)
        })
        .collect();
    (0..n)
        .map(|i| {
            let t = if n == 1 {
                1.0
            } else {
                i as f64 / (n - 1) as f64
            };
            let mut rgb = [0.0_f32; 3];
            for (c, (y, d)) in channels.iter().enumerate() {
                rgb[c] = pchip_eval(&x, y, d, t).clamp(0.0, 1.0) as f32;
            }
            rgb
        })
        .collect()
}

/// A named visibility preset over structures (`specs/11` §5.2).
#[derive(Clone, Debug, PartialEq)]
pub struct AtlasPreset {
    pub label: String,
    pub show: Vec<usize>,
    pub hide: Vec<usize>,
    /// Structures in neither list are hidden (`absolute`) or left unchanged.
    pub absolute: bool,
}

impl AtlasPreset {
    pub fn show_all(structures: Vec<usize>) -> Self {
        AtlasPreset {
            label: "Show all structures".into(),
            show: structures,
            hide: Vec::new(),
            absolute: true,
        }
    }

    pub fn hide_all() -> Self {
        AtlasPreset {
            label: "Hide all structures".into(),
            show: Vec::new(),
            hide: Vec::new(),
            absolute: true,
        }
    }

    /// Applies the preset to per-mesh flags; returns the meshes it turned on.
    pub fn apply(&self, flags: &mut [bool], mesh_structure: &[usize]) -> Vec<usize> {
        let mut turned_on = Vec::new();
        for (i, flag) in flags.iter_mut().enumerate() {
            let Some(s) = mesh_structure.get(i) else {
                continue;
            };
            let want = if self.hide.contains(s) {
                false
            } else if self.show.contains(s) {
                true
            } else if self.absolute {
                false
            } else {
                *flag
            };
            if want && !*flag {
                turned_on.push(i);
            }
            *flag = want;
        }
        turned_on
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AtlasStructureInfo {
    pub name: String,
    /// Atlas type code, `specs/11` §2.1.
    pub kind: usize,
}

/// Everything the side panel needs about one atlas set besides its meshes.
#[derive(Clone, Debug, Default)]
pub struct AtlasSetInfo {
    pub id: String,
    pub citation_name: Option<String>,
    pub citation_short: Option<String>,
    pub structures: Vec<AtlasStructureInfo>,
    /// Atlas structure index of every mesh: the index presets and label sets refer to (position
    /// in the atlas `names` list, which may have gaps for skipped entries).
    pub mesh_structure: Vec<usize>,
    pub mesh_side: Vec<Option<AtlasSideV1>>,
    /// The atlas's own presets (the two built-ins are not stored).
    pub presets: Vec<AtlasPreset>,
    pub default_preset: Option<usize>,
    /// Alternative structure names, aligned with `structures`.
    pub label_sets: Vec<(String, Vec<String>)>,
}

impl AtlasSetInfo {
    /// One structure per mesh, no presets (OBJ folders, label images, flat folders).
    pub fn per_mesh(id: &str, names: &[String]) -> Self {
        AtlasSetInfo {
            id: id.to_string(),
            structures: names
                .iter()
                .map(|n| AtlasStructureInfo {
                    name: n.clone(),
                    kind: 0,
                })
                .collect(),
            mesh_structure: (0..names.len()).collect(),
            mesh_side: vec![None; names.len()],
            ..Default::default()
        }
    }

    /// Distinct atlas structure indices that have a mesh, ascending.
    pub fn structure_indices(&self) -> Vec<usize> {
        let mut v = self.mesh_structure.clone();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Built-ins first, then the atlas presets (`specs/11` §5.2).
    pub fn preset_menu(&self) -> Vec<AtlasPreset> {
        let mut out = vec![
            AtlasPreset::show_all(self.structure_indices()),
            AtlasPreset::hide_all(),
        ];
        out.extend(self.presets.iter().cloned());
        out
    }

    /// Entry of [`Self::preset_menu`] matching the initial visibility, if any.
    pub fn initial_menu_entry(&self, initial: &[bool]) -> Option<usize> {
        if let Some(p) = self.default_preset.filter(|p| *p < self.presets.len()) {
            return Some(p + 2);
        }
        if initial.iter().all(|v| *v) {
            Some(0)
        } else if initial.iter().all(|v| !*v) {
            Some(1)
        } else {
            None
        }
    }

    /// Row text of a mesh: its tag, or the chosen label set's name plus the side.
    pub fn display_name(&self, mesh: usize, tag: &str, label_set: Option<usize>) -> String {
        let label = label_set
            .and_then(|k| self.label_sets.get(k))
            .zip(self.mesh_structure.get(mesh))
            .and_then(|((_, names), s)| names.get(*s))
            .filter(|n| !n.trim().is_empty());
        match (label, self.mesh_side.get(mesh).copied().flatten()) {
            (Some(l), Some(side)) => format!("{l} ({})", side_word(side)),
            (Some(l), None) => l.clone(),
            (None, _) => tag.to_string(),
        }
    }
}

fn side_word(side: AtlasSideV1) -> &'static str {
    match side {
        AtlasSideV1::Right => "right",
        AtlasSideV1::Left => "left",
        AtlasSideV1::Midline => "midline",
    }
}

/// Stub meshes (not yet meshed), their initial visibility and the set description.
pub struct BuiltAtlasSet {
    pub meshes: Vec<NamedAtlasMesh>,
    pub initial: Vec<bool>,
    pub info: AtlasSetInfo,
}

fn side_source(
    root: &Path,
    kind: usize,
    file: &str,
    side: AtlasSideV1,
    threshold: Option<f32>,
) -> Option<LeadRoiSource> {
    let (folder, split) = match (kind, side) {
        (4, AtlasSideV1::Right) => ("mixed", Some(HemisphereSplit::Right)),
        (4, AtlasSideV1::Left) => ("mixed", Some(HemisphereSplit::Left)),
        (5, _) | (_, AtlasSideV1::Midline) => ("midline", None),
        (1..=3 | 6, AtlasSideV1::Right) => ("rh", None),
        (1..=3 | 6, AtlasSideV1::Left) => ("lh", None),
        _ => return None,
    };
    Some(LeadRoiSource {
        path: root.join(folder).join(file),
        threshold,
        split,
    })
}

fn sides_of_kind(kind: usize) -> &'static [AtlasSideV1] {
    match kind {
        1 => &[AtlasSideV1::Right],
        2 => &[AtlasSideV1::Left],
        3 | 4 | 6 => &[AtlasSideV1::Right, AtlasSideV1::Left],
        5 => &[AtlasSideV1::Midline],
        _ => &[],
    }
}

fn default_tag(name: &str, side: AtlasSideV1) -> String {
    format!("{name}_{}", side_word(side))
}

fn name_matches(name: &str, subs: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    subs.iter().any(|t| lower.contains(&t.to_ascii_lowercase()))
}

fn explicit_substrings<'a>(
    set: &'a Option<Vec<String>>,
    global: &'a Option<Vec<String>>,
) -> Option<&'a [String]> {
    set.as_deref()
        .or(global.as_deref())
        .filter(|v| !v.is_empty())
}

const FLAT_FALLBACK_ON: [&str; 4] = ["STN", "GPi", "GPe", "RN"];

/// Stubs of one manifest lead set; nothing is meshed or read beyond the folder listing.
pub fn build_lead_set(
    set: &AtlasLeadSetV1,
    global_substrings: &Option<Vec<String>>,
) -> VizResult<BuiltAtlasSet> {
    let root = set.dir.as_path();
    let layout = set.layout.unwrap_or(if has_lead_side_folders(root) {
        AtlasLayoutV1::Lead
    } else {
        AtlasLayoutV1::Flat
    });
    let mut built = match (&set.index, layout) {
        (Some(index), _) => from_index(root, index),
        (None, AtlasLayoutV1::Lead) => {
            from_scan(root, &set.default_on_substrings, global_substrings)?
        }
        (None, AtlasLayoutV1::Flat) => {
            from_flat(root, &set.default_on_substrings, global_substrings)?
        }
    };
    built.info.id = set.set_id().to_string();
    Ok(built)
}

fn push_side(
    built: &mut BuiltAtlasSet,
    structure: usize,
    tag: String,
    side: AtlasSideV1,
    color: [f32; 3],
    alpha: f32,
    source: LeadRoiSource,
) {
    built.meshes.push(NamedAtlasMesh {
        name: tag,
        mesh: None,
        color: [color[0], color[1], color[2], alpha],
        source: Some(source),
    });
    built.info.mesh_structure.push(structure);
    built.info.mesh_side.push(Some(side));
}

fn empty_built() -> BuiltAtlasSet {
    BuiltAtlasSet {
        meshes: Vec::new(),
        initial: Vec::new(),
        info: AtlasSetInfo::default(),
    }
}

fn from_index(root: &Path, index: &AtlasIndexV1) -> BuiltAtlasSet {
    let palette = default_palette(index.structures.len());
    let mut built = empty_built();
    for (si, st) in index.structures.iter().enumerate() {
        let atlas_ix = st.index.unwrap_or(si);
        built.info.structures.push(AtlasStructureInfo {
            name: st.name.clone(),
            kind: st.kind,
        });
        let derived: Vec<AtlasStructureSideV1>;
        let sides: &[AtlasStructureSideV1] = if st.sides.is_empty() {
            derived = sides_of_kind(st.kind)
                .iter()
                .map(|&side| AtlasStructureSideV1 {
                    side,
                    tag: None,
                    color: None,
                    alpha: None,
                    threshold: None,
                })
                .collect();
            &derived
        } else {
            &st.sides
        };
        for side in sides {
            let Some(source) = side_source(root, st.kind, &st.file, side.side, side.threshold)
            else {
                crate::viz_eprintln!("[atlas] {}: kind {} not meshed", st.file, st.kind);
                continue;
            };
            let tag = side
                .tag
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| default_tag(&st.name, side.side));
            let color = side
                .color
                .filter(|c| c.iter().all(|v| v.is_finite()))
                .unwrap_or(palette[si]);
            let alpha = side
                .alpha
                .filter(|a| a.is_finite())
                .unwrap_or(DEFAULT_ATLAS_ALPHA)
                .clamp(0.0, 1.0);
            push_side(&mut built, atlas_ix, tag, side.side, color, alpha, source);
        }
    }
    built.info.citation_name = index.citation_name.clone();
    built.info.citation_short = index.citation_short.clone();
    built.info.presets = index
        .presets
        .iter()
        .map(|p| AtlasPreset {
            label: p.label.clone(),
            show: p.show.clone(),
            hide: p.hide.clone(),
            absolute: p.mode.eq_ignore_ascii_case("absolute"),
        })
        .collect();
    built.info.default_preset = index
        .default_preset
        .filter(|p| *p < built.info.presets.len());
    built.info.label_sets = index
        .labels
        .iter()
        .map(|l| (l.name.clone(), l.labels.clone()))
        .collect();
    built.initial = match built.info.default_preset {
        Some(p) => {
            let show = &built.info.presets[p].show;
            built
                .info
                .mesh_structure
                .iter()
                .map(|s| show.contains(s))
                .collect()
        }
        None => vec![true; built.meshes.len()],
    };
    built
}

fn from_scan(
    root: &Path,
    set_subs: &Option<Vec<String>>,
    global: &Option<Vec<String>>,
) -> VizResult<BuiltAtlasSet> {
    let scanned = scan_lead_layout(root)?;
    let palette = default_palette(scanned.len());
    let mut built = empty_built();
    for (si, st) in scanned.iter().enumerate() {
        let name = structure_name_of(&st.file);
        for &side in sides_of_kind(st.kind) {
            if let Some(source) = side_source(root, st.kind, &st.file, side, None) {
                let tag = default_tag(&name, side);
                push_side(
                    &mut built,
                    si,
                    tag,
                    side,
                    palette[si],
                    DEFAULT_ATLAS_ALPHA,
                    source,
                );
            }
        }
        built.info.structures.push(AtlasStructureInfo {
            name,
            kind: st.kind,
        });
    }
    let subs = explicit_substrings(set_subs, global);
    built.initial = built
        .info
        .mesh_structure
        .iter()
        .map(|&s| subs.is_none_or(|subs| name_matches(&built.info.structures[s].name, subs)))
        .collect();
    Ok(built)
}

fn from_flat(
    root: &Path,
    set_subs: &Option<Vec<String>>,
    global: &Option<Vec<String>>,
) -> VizResult<BuiltAtlasSet> {
    let items = collect_flat_atlas_items(root)?;
    let names: Vec<String> = items.iter().map(|(n, _)| n.clone()).collect();
    let meshes: Vec<NamedAtlasMesh> = items
        .into_iter()
        .map(|(name, path)| {
            let h: u32 = name.bytes().map(|b| b as u32).sum();
            NamedAtlasMesh {
                color: atlas_label_color(&name, h),
                name,
                mesh: None,
                source: Some(LeadRoiSource::whole(path)),
            }
        })
        .collect();
    let fallback: Vec<String> = FLAT_FALLBACK_ON.iter().map(|s| s.to_string()).collect();
    let initial = match (set_subs, global) {
        (Some(v), _) | (None, Some(v)) if v.is_empty() => vec![true; names.len()],
        (Some(v), _) | (None, Some(v)) => names.iter().map(|n| name_matches(n, v)).collect(),
        (None, None) => names.iter().map(|n| name_matches(n, &fallback)).collect(),
    };
    Ok(BuiltAtlasSet {
        meshes,
        initial,
        info: AtlasSetInfo::per_mesh("", &names),
    })
}

/// Index of the set shown by default: [`DEFAULT_ATLAS_SET_NAME`] when listed, else the first.
pub fn default_set_index<'a>(labels: impl IntoIterator<Item = &'a str>) -> usize {
    labels
        .into_iter()
        .position(|l| l == DEFAULT_ATLAS_SET_NAME)
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::scene_manifest::{AtlasPresetV1, AtlasStructureV1};

    fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol)
    }

    #[test]
    fn palette_matches_the_check_values() {
        let p4 = default_palette(4);
        let want4 = [
            [0.03, 0.353, 1.0],
            [0.8072, 0.8711, 1.0],
            [1.0, 1.0, 0.1333],
            [1.0, 0.0, 0.0],
        ];
        for (got, want) in p4.iter().zip(want4) {
            assert!(close(*got, want, 1e-3), "{got:?} vs {want:?}");
        }
        let p15 = default_palette(15);
        assert!(close(p15[0], [0.03, 0.353, 1.0], 1e-3));
        assert!(close(p15[7], [0.9902, 0.9935, 0.925], 1e-3), "{:?}", p15[7]);
        assert!(close(p15[14], [1.0, 0.0, 0.0], 1e-3));
        assert_eq!(default_palette(1), vec![[1.0, 0.0, 0.0]]);
        assert!(default_palette(0).is_empty());
    }

    fn side(side: AtlasSideV1, color: [f32; 3], threshold: f32) -> AtlasStructureSideV1 {
        AtlasStructureSideV1 {
            side,
            tag: None,
            color: Some(color),
            alpha: Some(0.7),
            threshold: Some(threshold),
        }
    }

    fn distal_like_index() -> AtlasIndexV1 {
        let names = ["STN", "STN_motor", "STN_associative", "STN_limbic", "GPi"];
        AtlasIndexV1 {
            citation_short: Some("Ewert et al. 2017".into()),
            structures: names
                .iter()
                .enumerate()
                .map(|(i, n)| AtlasStructureV1 {
                    index: Some(i),
                    file: format!("{n}.nii.gz"),
                    name: n.to_string(),
                    kind: 3,
                    tissue_type: Some(1.0),
                    sides: vec![
                        side(AtlasSideV1::Right, [1.0, 0.5, 0.0], 0.4),
                        side(AtlasSideV1::Left, [1.0, 0.5, 0.0], 0.4),
                    ],
                })
                .collect(),
            presets: vec![
                AtlasPresetV1 {
                    label: "Primary DBS targets".into(),
                    show: vec![0, 4],
                    hide: vec![],
                    mode: "absolute".into(),
                },
                AtlasPresetV1 {
                    label: "STN: Subzones".into(),
                    show: vec![1, 2, 3],
                    hide: vec![0],
                    mode: "relative".into(),
                },
            ],
            default_preset: Some(0),
            labels: vec![crate::scene_manifest::AtlasLabelSetV1 {
                name: "Names".into(),
                labels: vec![
                    "Subthalamic Nucleus".into(),
                    "".into(),
                    "".into(),
                    "".into(),
                    "Internal Pallidum".into(),
                ],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn index_structures_give_ordered_sides_colours_and_presets() {
        let set = AtlasLeadSetV1 {
            name: "DISTAL Minimal (Ewert 2017)".into(),
            dir: "/atlas".into(),
            id: Some("leaddbs:DISTAL".into()),
            layout: None,
            default_on_substrings: None,
            index: Some(distal_like_index()),
        };
        let built = build_lead_set(&set, &None).unwrap();
        assert_eq!(built.info.id, "leaddbs:DISTAL");
        assert_eq!(built.meshes.len(), 10);
        assert_eq!(built.meshes[0].name, "STN_right");
        assert_eq!(built.meshes[1].name, "STN_left");
        let src = built.meshes[0].source.as_ref().unwrap();
        assert!(src.path.ends_with(Path::new("rh").join("STN.nii.gz")));
        assert_eq!(src.threshold, Some(0.4));
        let left = built.meshes[1].source.as_ref().unwrap();
        assert!(left.path.ends_with(Path::new("lh").join("STN.nii.gz")));
        assert_eq!(built.meshes[0].color, [1.0, 0.5, 0.0, 0.7]);
        assert_eq!(
            built.initial,
            vec![true, true, false, false, false, false, false, false, true, true]
        );
        assert_eq!(built.info.initial_menu_entry(&built.initial), Some(2));
        let menu = built.info.preset_menu();
        assert_eq!(menu[0].label, "Show all structures");
        assert_eq!(menu[1].label, "Hide all structures");
        assert_eq!(menu[3].label, "STN: Subzones");
        assert_eq!(built.info.display_name(0, "STN_right", None), "STN_right");
        assert_eq!(
            built.info.display_name(9, "GPi_left", Some(0)),
            "Internal Pallidum (left)"
        );
        assert_eq!(
            built.info.display_name(2, "STN_motor_right", Some(0)),
            "STN_motor_right"
        );
    }

    #[test]
    fn presets_follow_the_absolute_and_relative_rules() {
        let info = from_index(Path::new("/a"), &distal_like_index()).info;
        let mut flags = vec![true; 10];
        flags[8] = false;
        let on = info.presets[1].apply(&mut flags, &info.mesh_structure);
        assert_eq!(
            flags,
            vec![false, false, true, true, true, true, true, true, false, true],
            "relative: STN hidden, subzones shown, GPi left as it was"
        );
        assert!(on.is_empty());
        let on = info.presets[0].apply(&mut flags, &info.mesh_structure);
        assert_eq!(
            flags,
            vec![true, true, false, false, false, false, false, false, true, true]
        );
        assert_eq!(on, vec![0, 1, 8]);
        AtlasPreset::hide_all().apply(&mut flags, &info.mesh_structure);
        assert!(flags.iter().all(|f| !f));
        let on = AtlasPreset::show_all(vec![0, 1, 2, 3, 4]).apply(&mut flags, &info.mesh_structure);
        assert_eq!(on.len(), 10);
    }

    #[test]
    fn index_without_presets_shows_everything_and_mixed_or_midline_resolve_their_folders() {
        let mut index = distal_like_index();
        index.presets.clear();
        index.default_preset = None;
        index.structures.truncate(1);
        index.structures[0].kind = 4;
        index.structures.push(AtlasStructureV1 {
            index: None,
            file: "Fornix.nii".into(),
            name: "Fornix".into(),
            kind: 5,
            tissue_type: None,
            sides: vec![],
        });
        index.structures.push(AtlasStructureV1 {
            index: None,
            file: "map.nii".into(),
            name: "map".into(),
            kind: 10,
            tissue_type: None,
            sides: vec![],
        });
        let built = from_index(Path::new("/a"), &index);
        assert_eq!(built.meshes.len(), 3);
        assert!(built.initial.iter().all(|v| *v));
        let s0 = built.meshes[0].source.as_ref().unwrap();
        assert!(s0.path.ends_with(Path::new("mixed").join("STN.nii.gz")));
        assert_eq!(s0.split, Some(HemisphereSplit::Right));
        assert_eq!(
            built.meshes[1].source.as_ref().unwrap().split,
            Some(HemisphereSplit::Left)
        );
        assert_eq!(built.meshes[2].name, "Fornix_midline");
        assert!(built.meshes[2]
            .source
            .as_ref()
            .unwrap()
            .path
            .ends_with(Path::new("midline").join("Fornix.nii")));
        assert_eq!(built.info.structures.len(), 3);
        assert_eq!(built.meshes[2].color[..3], default_palette(3)[1]);
    }

    #[test]
    fn a_scanned_lead_folder_pairs_sides_in_spec_order() {
        let tmp = std::env::temp_dir().join(format!("dbsloc_atlas_scan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for (sub, file) in [
            ("lh", "STN.nii.gz"),
            ("rh", "STN.nii.gz"),
            ("lh", "GPe.nii"),
            ("rh", "GPe.nii"),
            ("rh", "RightOnly.nii"),
            ("lh", "LeftOnly.nii"),
            ("lh", "scratch_temp.nii"),
            ("mixed", "Ca.nii.gz"),
            ("midline", "Fornix.nii"),
        ] {
            std::fs::create_dir_all(tmp.join(sub)).unwrap();
            std::fs::write(tmp.join(sub).join(file), b"x").unwrap();
        }
        let set = AtlasLeadSetV1 {
            name: "scan".into(),
            dir: tmp.clone(),
            id: None,
            layout: None,
            default_on_substrings: None,
            index: None,
        };
        let built = build_lead_set(&set, &None).unwrap();
        let names: Vec<&str> = built.meshes.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "RightOnly_right",
                "LeftOnly_left",
                "GPe_right",
                "GPe_left",
                "STN_right",
                "STN_left",
                "Ca_right",
                "Ca_left",
                "Fornix_midline"
            ]
        );
        assert_eq!(built.info.id, "scan");
        assert!(built.initial.iter().all(|v| *v));
        let palette = default_palette(6);
        assert_eq!(built.meshes[2].color[..3], palette[2]);
        assert_eq!(built.meshes[3].color[..3], palette[2]);
        assert_eq!(built.meshes[3].color[3], DEFAULT_ATLAS_ALPHA);
        assert_eq!(built.info.mesh_structure, vec![0, 1, 2, 2, 3, 3, 4, 4, 5]);
        let only_stn = AtlasLeadSetV1 {
            default_on_substrings: Some(vec!["stn".into()]),
            ..set
        };
        let built = build_lead_set(&only_stn, &None).unwrap();
        assert_eq!(
            built.initial,
            vec![false, false, false, false, true, true, false, false, false]
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn presets_and_labels_use_the_atlas_index_across_skipped_entries() {
        let mut index = distal_like_index();
        index.structures.truncate(2);
        index.structures[1].index = Some(3);
        index.presets = vec![AtlasPresetV1 {
            label: "Second only".into(),
            show: vec![3],
            hide: vec![0],
            mode: "relative".into(),
        }];
        index.labels[0].labels = vec!["A".into(), "x".into(), "y".into(), "D".into()];
        let built = from_index(Path::new("/a"), &index);
        assert_eq!(built.info.mesh_structure, vec![0, 0, 3, 3]);
        assert_eq!(built.initial, vec![false, false, true, true]);
        assert_eq!(built.info.display_name(2, "t", Some(0)), "D (right)");
        let menu = built.info.preset_menu();
        assert_eq!(menu[0].show, vec![0, 3]);
        let mut flags = vec![false; 4];
        assert_eq!(
            menu[0].apply(&mut flags, &built.info.mesh_structure).len(),
            4
        );
    }

    fn distal_minimal_dir() -> Option<std::path::PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../resources/atlases/leaddbs/DISTAL Minimal (Ewert 2017)");
        if dir.join("rh").join("STN.nii.gz").is_file() {
            Some(dir)
        } else {
            eprintln!("skipped: DISTAL Minimal not found at {}", dir.display());
            None
        }
    }

    fn bbox(mesh: &crate::atlas_wavefront::TriMeshScene) -> ([f32; 3], [f32; 3]) {
        mesh.positions
            .iter()
            .fold(([f32::MAX; 3], [f32::MIN; 3]), |(mn, mx), p| {
                (
                    [mn[0].min(p.x), mn[1].min(p.y), mn[2].min(p.z)],
                    [mx[0].max(p.x), mx[1].max(p.y), mx[2].max(p.z)],
                )
            })
    }

    fn within(got: [f32; 3], want: [f32; 3], tol: f32) -> bool {
        got.iter().zip(want).all(|(g, w)| (g - w).abs() <= tol)
    }

    #[test]
    fn distal_minimal_stn_right_matches_the_reference_mesh() {
        use crate::atlas_marching::{
            mesh_binary_mask_nifti_cached_with_params, mesh_binary_mask_nifti_with_params,
            prepare_mask_values, AtlasMeshCacheDirs, LeadAtlasMeshParams,
        };
        let Some(dir) = distal_minimal_dir() else {
            return;
        };
        let mut index = distal_like_index();
        index.structures.truncate(1);
        let set = AtlasLeadSetV1 {
            name: DEFAULT_ATLAS_SET_NAME.into(),
            dir: dir.clone(),
            id: None,
            layout: Some(AtlasLayoutV1::Lead),
            default_on_substrings: None,
            index: Some(index),
        };
        let built = build_lead_set(&set, &None).unwrap();
        let stn = &built.meshes[0];
        assert_eq!(stn.name, "STN_right");
        assert!(close(
            [stn.color[0], stn.color[1], stn.color[2]],
            [1.0, 0.5, 0.0],
            1e-4
        ));
        assert!((stn.color[3] - 0.7).abs() < 1e-6);
        let src = stn.source.clone().unwrap();

        let vol = dbsloc_checkreg::nifti_io::read_volume_f32(&src.path).unwrap();
        let mut data = vol.data;
        let level = prepare_mask_values(&mut data, src.threshold).unwrap();
        assert_eq!(data.iter().filter(|v| **v > level).count(), 16215);

        let params = LeadAtlasMeshParams {
            template: None,
            resample_to_template: false,
            mesh_smooth_iters: 0,
            source_ijk_subtract: 0.0,
            native_vertex_ijk_shift: 0.0,
        };
        let raw = mesh_binary_mask_nifti_with_params(&src, "STN_right", &params).unwrap();
        let (mn, mx) = bbox(raw.mesh.as_ref().unwrap());
        assert!(
            within(mn, [6.673, -17.988, -11.503], 0.05),
            "raw min {mn:?}"
        );
        assert!(within(mx, [15.871, -8.139, -3.972], 0.05), "raw max {mx:?}");

        let tmp = std::env::temp_dir().join(format!("dbsloc_distal_stn_{}", std::process::id()));
        let cache = AtlasMeshCacheDirs {
            shipped: tmp.join("none"),
            user: None,
        };
        let started = std::time::Instant::now();
        let shown = mesh_binary_mask_nifti_cached_with_params(
            &src,
            "STN_right",
            &params,
            &cache,
            None,
            6,
            0,
            0.05,
            10,
            0.5,
            -0.53,
        )
        .unwrap();
        eprintln!("STN right meshed in {:?}", started.elapsed());
        let mesh = shown.mesh.unwrap();
        let centroid =
            mesh.positions.iter().copied().sum::<glam::Vec3>() / mesh.positions.len() as f32;
        assert!(
            within(centroid.to_array(), [11.49, -12.61, -7.62], 0.3),
            "centroid {centroid:?}"
        );
        let (mn, mx) = bbox(&mesh);
        assert!(within(mn, [6.7, -17.9, -11.4], 0.5), "min {mn:?}");
        assert!(within(mx, [15.8, -8.2, -4.1], 0.5), "max {mx:?}");
        eprintln!(
            "STN right: {} vertices, centroid {centroid:?}, bbox {mn:?}..{mx:?}",
            mesh.positions.len()
        );
    }

    #[test]
    fn default_set_prefers_distal_minimal() {
        assert_eq!(default_set_index(["AHEAD", DEFAULT_ATLAS_SET_NAME]), 1);
        assert_eq!(default_set_index(["AHEAD", "Other"]), 0);
    }
}
