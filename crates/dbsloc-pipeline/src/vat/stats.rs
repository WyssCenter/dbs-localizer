// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Atlas overlap statistics of a VTA (`specs/07` §8): per structure and hemisphere the
//! intersection with the binary VTA and the E-field overlap, the JSON record and the
//! Lead-DBS compatible `ea_stats` MAT variable.

use std::path::{Path, PathBuf};

use fastwarp_core::{apply_rowmajor, invert_rowmajor, Mat4};
use fastwarp_matfile::mat5::{mat5_bytes, read_mat5, Mat5Value};
use rayon::prelude::*;
use serde_json::{json, Value};

use crate::atlas_catalog::{default_atlas, AtlasLayout, AtlasSetEntry};
use crate::nifti_io::load_volume_f64;
use crate::vat::settings::Side;
use crate::vat::warp::Field;

pub const EA_STATS_VARIABLE: &str = "ea_stats";

/// One entry of the atlas order (`atlases.names`).
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub file: String,
    pub kind: Option<i64>,
    pub skipped: bool,
}

/// The atlas set the statistics use.
#[derive(Debug, Clone)]
pub struct StatsAtlas {
    pub id: String,
    pub name: String,
    pub citation: Option<String>,
    pub dir: PathBuf,
    pub layout: AtlasLayout,
    pub entries: Vec<Entry>,
}

fn is_volume_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    (n.ends_with(".nii") || n.ends_with(".nii.gz")) && !n.contains("_temp.nii")
}

fn listing(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| is_volume_name(n))
        .collect();
    names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    names
}

fn entries_without_index(set: &AtlasSetEntry) -> Vec<Entry> {
    let volume = |file: String, kind: i64| Entry {
        file,
        kind: Some(kind),
        skipped: false,
    };
    if set.layout == AtlasLayout::Flat {
        return listing(&set.dir)
            .into_iter()
            .map(|f| volume(f, 4))
            .collect();
    }
    let rh = listing(&set.dir.join("rh"));
    let lh = listing(&set.dir.join("lh"));
    let mut out: Vec<Entry> = Vec::new();
    out.extend(
        rh.iter()
            .filter(|f| !lh.contains(f))
            .map(|f| volume(f.clone(), 1)),
    );
    out.extend(
        lh.iter()
            .filter(|f| !rh.contains(f))
            .map(|f| volume(f.clone(), 2)),
    );
    out.extend(
        lh.iter()
            .filter(|f| rh.contains(f))
            .map(|f| volume(f.clone(), 3)),
    );
    out.extend(
        listing(&set.dir.join("mixed"))
            .into_iter()
            .map(|f| volume(f, 4)),
    );
    out.extend(
        listing(&set.dir.join("midline"))
            .into_iter()
            .map(|f| volume(f, 5)),
    );
    out
}

impl StatsAtlas {
    /// The atlas set named by `choice` (id or name), else the default set.
    pub fn resolve(sets: &[AtlasSetEntry], choice: Option<&str>) -> Result<StatsAtlas, String> {
        let set = match choice {
            Some(c) => sets
                .iter()
                .find(|e| e.id == c || e.name == c)
                .ok_or_else(|| format!("atlas set {c:?} is not available"))?,
            None => default_atlas(sets).ok_or_else(|| "no atlas set is available".to_owned())?,
        };
        let entries = match &set.index {
            Some(index) => {
                let n = index.structures.len() + index.skipped.len();
                let mut slots: Vec<Option<Entry>> = vec![None; n];
                for s in &index.structures {
                    if let Some(slot) = slots.get_mut(s.index) {
                        *slot = Some(Entry {
                            file: s.file.clone(),
                            kind: Some(s.kind),
                            skipped: false,
                        });
                    }
                }
                for s in &index.skipped {
                    if let Some(slot) = slots.get_mut(s.index) {
                        *slot = Some(Entry {
                            file: s.file.clone(),
                            kind: None,
                            skipped: true,
                        });
                    }
                }
                slots
                    .into_iter()
                    .map(|s| {
                        s.unwrap_or(Entry {
                            file: String::new(),
                            kind: None,
                            skipped: true,
                        })
                    })
                    .collect()
            }
            None => entries_without_index(set),
        };
        Ok(StatsAtlas {
            id: set.id.clone(),
            name: set.name.clone(),
            citation: set.citation.clone(),
            dir: set.dir.clone(),
            layout: set.layout,
            entries,
        })
    }

    pub fn path_for(&self, entry: &Entry, side: Side) -> Option<PathBuf> {
        if entry.skipped || entry.file.is_empty() {
            return None;
        }
        if self.layout == AtlasLayout::Flat {
            return Some(self.dir.join(&entry.file));
        }
        let folder = match (entry.kind.unwrap_or(3), side) {
            (1, Side::Right) | (3 | 6, Side::Right) => "rh",
            (2, Side::Left) | (3 | 6, Side::Left) => "lh",
            (4, _) => "mixed",
            (5, _) => "midline",
            (1, Side::Left) | (2, Side::Right) => return None,
            (_, Side::Right) => "rh",
            (_, Side::Left) => "lh",
        };
        Some(self.dir.join(folder).join(&entry.file))
    }

    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.file.clone()).collect()
    }
}

/// A binarised atlas structure on its own grid.
#[derive(Debug, Clone)]
pub struct AtlasMask {
    dims: [usize; 3],
    inverse: Mat4,
    data: Vec<u8>,
    pub volume_mm3: f64,
}

fn det3(m: &Mat4) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

impl AtlasMask {
    /// Loads a structure file, splits it at world x = 0 for `side` and binarises at half of the
    /// maximum of the split image (`specs/07` §8.1).
    pub fn load(path: &Path, side: Side) -> Result<Option<AtlasMask>, String> {
        let (vol, aff) = load_volume_f64(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (nx, ny, nz) = vol.dim();
        let mut split = vec![0f64; nx * ny * nz];
        let mut max = 0f64;
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    let v = vol[[i, j, k]];
                    if !v.is_finite() {
                        continue;
                    }
                    let x = apply_rowmajor(&aff, [i as f64, j as f64, k as f64])[0];
                    let keep = match side {
                        Side::Right => x >= 0.0,
                        Side::Left => x <= 0.0,
                    };
                    if keep {
                        split[i + nx * (j + ny * k)] = v;
                        max = max.max(v);
                    }
                }
            }
        }
        let inverse =
            invert_rowmajor(&aff).ok_or_else(|| format!("{}: singular affine", path.display()))?;
        let cut = 0.5 * max;
        let data: Vec<u8> = split
            .iter()
            .map(|&v| u8::from(max > 0.0 && v > cut))
            .collect();
        let count = data.iter().filter(|&&v| v != 0).count();
        Ok(Some(AtlasMask {
            dims: [nx, ny, nz],
            inverse,
            data,
            volume_mm3: count as f64 * det3(&aff).abs(),
        }))
    }

    fn at(&self, i: i64, j: i64, k: i64) -> f64 {
        if i < 0 || j < 0 || k < 0 {
            return 0.0;
        }
        let (i, j, k) = (i as usize, j as usize, k as usize);
        if i >= self.dims[0] || j >= self.dims[1] || k >= self.dims[2] {
            return 0.0;
        }
        f64::from(self.data[i + self.dims[0] * (j + self.dims[1] * k)])
    }

    /// Trilinear value of the 0 / 1 mask at a world point.
    pub fn trilinear(&self, world: [f64; 3]) -> f64 {
        let p = apply_rowmajor(&self.inverse, world);
        let lo = [p[0].floor(), p[1].floor(), p[2].floor()];
        let f = [p[0] - lo[0], p[1] - lo[1], p[2] - lo[2]];
        let (i0, j0, k0) = (lo[0] as i64, lo[1] as i64, lo[2] as i64);
        let mut acc = 0.0;
        for c in 0..8i64 {
            let (di, dj, dk) = (c & 1, (c >> 1) & 1, (c >> 2) & 1);
            let w = (if di == 1 { f[0] } else { 1.0 - f[0] })
                * (if dj == 1 { f[1] } else { 1.0 - f[1] })
                * (if dk == 1 { f[2] } else { 1.0 - f[2] });
            if w > 0.0 {
                acc += w * self.at(i0 + di, j0 + dj, k0 + dk);
            }
        }
        acc
    }

    /// Whether the resampled mask contains the world point (> 0.5).
    pub fn contains(&self, world: [f64; 3]) -> bool {
        self.trilinear(world) > 0.5
    }
}

/// Overlap numbers of one hemisphere, one value per atlas entry.
#[derive(Debug, Clone, PartialEq)]
pub struct SideStats {
    pub side: Side,
    pub amplitudes: [f64; 4],
    pub volume_mm3: f64,
    pub atlas_volume: Vec<f64>,
    pub intersection: Vec<f64>,
    pub n_intersection: Vec<f64>,
    pub n_within: Vec<f64>,
    pub efield_volume: f64,
    pub efield_integral_vmm3: f64,
    pub efield_intersection: Vec<f64>,
    pub efield_n_intersection: Vec<f64>,
}

impl SideStats {
    /// A hemisphere without stimulation: zeros and NaN atlas volumes (`specs/07` §8.4).
    pub fn empty(side: Side, n: usize, amplitudes: [f64; 4]) -> SideStats {
        SideStats {
            side,
            amplitudes,
            volume_mm3: 0.0,
            atlas_volume: vec![f64::NAN; n],
            intersection: vec![0.0; n],
            n_intersection: vec![0.0; n],
            n_within: vec![0.0; n],
            efield_volume: 0.0,
            efield_integral_vmm3: 0.0,
            efield_intersection: vec![0.0; n],
            efield_n_intersection: vec![0.0; n],
        }
    }
}

fn voxel_volume(aff: &Mat4) -> f64 {
    det3(aff).abs()
}

/// Overlap of one hemisphere's template field with every atlas structure.
pub fn side_stats(
    atlas: &StatsAtlas,
    side: Side,
    field: &Field,
    thr_v_per_m: f32,
    amplitudes: [f64; 4],
    log: &(dyn Fn(String) + Sync),
) -> SideStats {
    let n = atlas.entries.len();
    let mut out = SideStats::empty(side, n, amplitudes);
    out.volume_mm3 = field.final_mm3;
    let vox = voxel_volume(&field.affine);
    let [nx, ny, nz] = field.dims;
    let has_field = !field.e_mag.is_empty();
    let mut cand: Vec<(usize, [f64; 3])> = Vec::new();
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let idx = i + nx * (j + ny * k);
                let hit = if has_field {
                    field.e_mag[idx] > thr_v_per_m
                } else {
                    field.binary[idx] != 0
                };
                if hit {
                    cand.push((
                        idx,
                        apply_rowmajor(&field.affine, [i as f64, j as f64, k as f64]),
                    ));
                }
            }
        }
    }
    let b_count = field.binary.iter().filter(|&&v| v != 0).count() as f64;
    let e_total: f64 = if has_field {
        cand.iter()
            .map(|(idx, _)| f64::from(field.e_mag[*idx]))
            .sum()
    } else {
        0.0
    };
    out.efield_volume = e_total;
    out.efield_integral_vmm3 = e_total * vox;
    type Row = (f64, f64, f64, f64, f64, f64);
    let rows: Vec<Row> = atlas
        .entries
        .par_iter()
        .map(|entry| {
            let nan_row: Row = (f64::NAN, 0.0, 0.0, 0.0, 0.0, 0.0);
            let Some(path) = atlas.path_for(entry, side) else {
                return nan_row;
            };
            if !path.is_file() {
                return nan_row;
            }
            let mask = match AtlasMask::load(&path, side) {
                Ok(Some(m)) => m,
                Ok(None) => return nan_row,
                Err(e) => {
                    log(format!(
                        "dbsloc: warning: vat: atlas structure skipped ({e})"
                    ));
                    return nan_row;
                }
            };
            let mut inter = 0usize;
            let mut e_inter = 0.0f64;
            for (idx, w) in &cand {
                if mask.contains(*w) {
                    if field.binary[*idx] != 0 {
                        inter += 1;
                    }
                    if has_field {
                        e_inter += f64::from(field.e_mag[*idx]);
                    }
                }
            }
            let inter_mm3 = inter as f64 * vox;
            let n_inter = if b_count > 0.0 {
                inter as f64 / b_count
            } else {
                0.0
            };
            let n_within = if mask.volume_mm3 > 0.0 {
                inter_mm3 / mask.volume_mm3
            } else {
                0.0
            };
            let en = if e_total > 0.0 {
                e_inter / e_total
            } else {
                0.0
            };
            (mask.volume_mm3, inter_mm3, n_inter, n_within, e_inter, en)
        })
        .collect();
    for (i, r) in rows.into_iter().enumerate() {
        out.atlas_volume[i] = r.0;
        out.intersection[i] = r.1;
        out.n_intersection[i] = r.2;
        out.n_within[i] = r.3;
        out.efield_intersection[i] = r.4;
        out.efield_n_intersection[i] = r.5;
    }
    out
}

fn num_array(v: &[f64]) -> Value {
    Value::Array(
        v.iter()
            .map(|x| if x.is_finite() { json!(x) } else { Value::Null })
            .collect(),
    )
}

/// The `sub-<id>_desc-vtastats.json` record (`specs/07` §8.4).
pub fn stats_json(
    atlas: &StatsAtlas,
    label: &str,
    model: &str,
    settings_sha256: &str,
    sides: &[SideStats],
) -> Value {
    let mut per_side = serde_json::Map::new();
    for s in sides {
        per_side.insert(
            s.side.name().to_owned(),
            json!({
                "amplitudes": s.amplitudes,
                "volume_mm3": s.volume_mm3,
                "atlas_volume_mm3": num_array(&s.atlas_volume),
                "intersection_mm3": num_array(&s.intersection),
                "n_intersection": num_array(&s.n_intersection),
                "n_within_intersection": num_array(&s.n_within),
                "efield": {
                    "volume": s.efield_volume,
                    "integral_v_per_m_mm3": s.efield_integral_vmm3,
                    "intersection": num_array(&s.efield_intersection),
                    "n_intersection": num_array(&s.efield_n_intersection),
                },
            }),
        );
    }
    json!({
        "schema": "dbsloc-vtastats",
        "schema_version": 1,
        "label": label,
        "model": model,
        "settings_sha256": settings_sha256,
        "atlas": {
            "id": atlas.id,
            "name": atlas.name,
            "citation": atlas.citation,
            "structures": atlas.entries.iter().map(|e| json!({
                "name": e.file,
                "type": e.kind,
                "skipped": e.skipped,
            })).collect::<Vec<_>>(),
        },
        "sides": Value::Object(per_side),
    })
}

fn named(fields: Vec<(&str, Mat5Value)>) -> Vec<(String, Mat5Value)> {
    fields.into_iter().map(|(n, v)| (n.to_owned(), v)).collect()
}

fn struct_array(dims: Vec<usize>, fields: &[&str], elements: Vec<Vec<Mat5Value>>) -> Mat5Value {
    Mat5Value::Struct {
        dims,
        fields: fields.iter().map(|f| (*f).to_owned()).collect(),
        elements,
    }
}

fn vat_entry(label: &str, s: &SideStats) -> Vec<Mat5Value> {
    vec![
        Mat5Value::row(&s.amplitudes),
        Mat5Value::char(label),
        Mat5Value::scalar(1.0),
        Mat5Value::scalar(s.side.index() as f64 + 1.0),
        Mat5Value::scalar(s.volume_mm3),
        Mat5Value::row(&s.atlas_volume),
        Mat5Value::row(&s.intersection),
        Mat5Value::row(&s.n_intersection),
        Mat5Value::row(&s.n_within),
    ]
}

fn efield_entry(s: &SideStats) -> Vec<Mat5Value> {
    vec![
        Mat5Value::scalar(s.efield_volume),
        Mat5Value::row(&s.efield_intersection),
        Mat5Value::row(&s.efield_n_intersection),
    ]
}

const VAT_FIELDS: [&str; 9] = [
    "amp",
    "label",
    "contact",
    "side",
    "volume",
    "AtlasVolume",
    "AtlasIntersection",
    "nAtlasIntersection",
    "nWithinAtlasIntersection",
];
const EFIELD_FIELDS: [&str; 3] = ["volume", "AtlasIntersection", "nAtlasIntersection"];
const STIM_FIELDS: [&str; 3] = ["label", "vat", "efield"];

fn stimulation_entry(label: &str, sides: &[SideStats; 2]) -> Vec<Mat5Value> {
    let vat = struct_array(
        vec![2, 1],
        &VAT_FIELDS,
        sides.iter().map(|s| vat_entry(label, s)).collect(),
    );
    let efield = struct_array(
        vec![2, 1],
        &EFIELD_FIELDS,
        sides.iter().map(efield_entry).collect(),
    );
    vec![Mat5Value::char(label), vat, efield]
}

fn atlases_value(atlas: &StatsAtlas) -> Mat5Value {
    let types: Vec<f64> = atlas
        .entries
        .iter()
        .map(|e| e.kind.map_or(f64::NAN, |k| k as f64))
        .collect();
    Mat5Value::struct_scalar(named(vec![
        (
            "names",
            Mat5Value::cell_row(atlas.names().iter().map(|n| Mat5Value::char(n)).collect()),
        ),
        ("types", Mat5Value::row(&types)),
    ]))
}

/// Why `ea_stats` could not be updated in place.
pub type Reason = String;

fn names_of(v: &Mat5Value) -> Option<Vec<String>> {
    match v {
        Mat5Value::Cell { items, .. } => items
            .iter()
            .map(|i| match i {
                Mat5Value::Char(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn upsert_stimulation(
    existing: &Mat5Value,
    label: &str,
    sides: &[SideStats; 2],
) -> Result<Mat5Value, Reason> {
    let Mat5Value::Struct {
        dims,
        fields,
        elements,
    } = existing
    else {
        return Err("ea_stats.stimulation is not a struct array".to_owned());
    };
    let mut fields = fields.clone();
    let mut elements = elements.clone();
    for f in STIM_FIELDS {
        if !fields.iter().any(|n| n == f) {
            fields.push(f.to_owned());
            for e in &mut elements {
                e.push(Mat5Value::Empty);
            }
        }
    }
    let label_col = fields
        .iter()
        .position(|n| n == "label")
        .ok_or("no label field")?;
    let new = stimulation_entry(label, sides);
    let build = |fields: &[String]| -> Vec<Mat5Value> {
        fields
            .iter()
            .map(|f| {
                STIM_FIELDS
                    .iter()
                    .position(|s| s == f)
                    .map_or(Mat5Value::Empty, |p| new[p].clone())
            })
            .collect()
    };
    let found = elements
        .iter()
        .position(|e| matches!(e.get(label_col), Some(Mat5Value::Char(s)) if s == label));
    match found {
        Some(t) => elements[t] = build(&fields),
        None => elements.push(build(&fields)),
    }
    let count = elements.len();
    let _ = dims;
    Ok(Mat5Value::Struct {
        dims: vec![1, count],
        fields,
        elements,
    })
}

fn fresh(atlas: &StatsAtlas, label: &str, sides: &[SideStats; 2]) -> Mat5Value {
    let stim = Mat5Value::Struct {
        dims: vec![1, 1],
        fields: STIM_FIELDS.iter().map(|s| (*s).to_owned()).collect(),
        elements: vec![stimulation_entry(label, sides)],
    };
    Mat5Value::struct_scalar(named(vec![
        ("atlases", atlases_value(atlas)),
        ("stimulation", stim),
    ]))
}

/// The bytes of the new `sub-<id>_desc-stats.mat`, or why the file at `path` is left alone.
pub fn ea_stats_bytes(
    path: &Path,
    atlas: &StatsAtlas,
    label: &str,
    sides: &[SideStats; 2],
) -> Result<Vec<u8>, Reason> {
    let bytes_of = |vars: &[(String, Mat5Value)]| {
        mat5_bytes(vars, true).map_err(|e| format!("cannot encode: {e}"))
    };
    if !path.is_file() {
        return bytes_of(&[(EA_STATS_VARIABLE.to_owned(), fresh(atlas, label, sides))]);
    }
    let raw = std::fs::read(path).map_err(|e| format!("cannot read: {e}"))?;
    let header = String::from_utf8_lossy(&raw[..raw.len().min(124)]).into_owned();
    if header.contains("7.3") {
        return Err("the file is MAT v7.3, which is not rewritten".to_owned());
    }
    let mut vars = read_mat5(&raw).map_err(|e| format!("cannot parse: {e}"))?;
    let Some(slot) = vars.iter_mut().find(|(n, _)| n == EA_STATS_VARIABLE) else {
        vars.push((EA_STATS_VARIABLE.to_owned(), fresh(atlas, label, sides)));
        return bytes_of(&vars);
    };
    let Mat5Value::Struct {
        fields, elements, ..
    } = &mut slot.1
    else {
        return Err("ea_stats is not a struct".to_owned());
    };
    if elements.len() != 1 {
        return Err("ea_stats is not a 1x1 struct".to_owned());
    }
    let col = |name: &str| fields.iter().position(|n| n == name);
    let atlases_col = col("atlases").ok_or("ea_stats has no atlases field")?;
    let theirs = match &elements[0][atlases_col] {
        Mat5Value::Struct {
            fields: f,
            elements: e,
            ..
        } if e.len() == 1 => f
            .iter()
            .position(|n| n == "names")
            .and_then(|p| names_of(&e[0][p])),
        _ => None,
    };
    if theirs.as_deref() != Some(atlas.names().as_slice()) {
        return Err("its atlas names differ from the statistics atlas".to_owned());
    }
    match col("stimulation") {
        Some(sc) => {
            let updated = upsert_stimulation(&elements[0][sc], label, sides)?;
            elements[0][sc] = updated;
        }
        None => {
            fields.push("stimulation".to_owned());
            let stim = Mat5Value::Struct {
                dims: vec![1, 1],
                fields: STIM_FIELDS.iter().map(|s| (*s).to_owned()).collect(),
                elements: vec![stimulation_entry(label, sides)],
            };
            elements[0].push(stim);
        }
    }
    bytes_of(&vars)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use fastwarp_matfile::mat5::read_mat5;

    fn atlas(n: usize) -> StatsAtlas {
        StatsAtlas {
            id: "t".into(),
            name: "T".into(),
            citation: None,
            dir: PathBuf::new(),
            layout: AtlasLayout::Lead,
            entries: (0..n)
                .map(|i| Entry {
                    file: format!("s{i}.nii.gz"),
                    kind: Some(3),
                    skipped: false,
                })
                .collect(),
        }
    }

    fn sides(n: usize) -> [SideStats; 2] {
        [
            SideStats::empty(Side::Right, n, [1.0, 0.0, 0.0, 0.0]),
            SideStats::empty(Side::Left, n, [0.0; 4]),
        ]
    }

    #[test]
    fn a_new_ea_stats_file_has_atlases_and_one_stimulation() {
        let d = std::env::temp_dir().join(format!("dbsloc_vat_ea_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("stats.mat");
        let a = atlas(2);
        let bytes = ea_stats_bytes(&p, &a, "x", &sides(2)).unwrap();
        std::fs::write(&p, &bytes).unwrap();
        let vars = read_mat5(&bytes).unwrap();
        assert_eq!(vars[0].0, "ea_stats");
        let stim = vars[0].1.field("stimulation", 0).unwrap();
        assert_eq!(stim.dims(), vec![1, 1]);
        let vat = stim.field("vat", 0).unwrap();
        assert_eq!(vat.dims(), vec![2, 1]);
        assert_eq!(vat.field("volume", 0).unwrap().dims(), vec![1, 1]);
        let again = ea_stats_bytes(&p, &a, "y", &sides(2)).unwrap();
        let vars = read_mat5(&again).unwrap();
        assert_eq!(
            vars[0].1.field("stimulation", 0).unwrap().dims(),
            vec![1, 2]
        );
        let same = ea_stats_bytes(&p, &a, "y", &sides(2)).unwrap();
        let vars = read_mat5(&same).unwrap();
        assert_eq!(
            vars[0].1.field("stimulation", 0).unwrap().dims(),
            vec![1, 1 + 1]
        );
        let other = atlas(3);
        assert!(ea_stats_bytes(&p, &other, "z", &sides(3)).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_mask_is_split_at_the_midline_and_binarised_at_half_the_maximum() {
        let d = std::env::temp_dir().join(format!("dbsloc_vat_mask_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let aff = [
            [1.0, 0.0, 0.0, -5.5],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut data = ndarray::Array3::<u8>::zeros((12, 3, 3));
        for i in 0..12usize {
            for j in 0..3usize {
                for k in 0..3usize {
                    data[[i, j, k]] = if i % 2 == 0 { 8 } else { 3 };
                }
            }
        }
        let p = d.join("m.nii");
        crate::nifti_io::write_u8_3d(&p, &data, &aff).unwrap();
        let right = AtlasMask::load(&p, Side::Right).unwrap().unwrap();
        assert!(
            (right.volume_mm3 - 3.0 * 3.0 * 3.0).abs() < 1e-9,
            "{}",
            right.volume_mm3
        );
        assert!(right.contains([0.5, 1.0, 1.0]));
        assert!(!right.contains([-2.5, 1.0, 1.0]));
        let left = AtlasMask::load(&p, Side::Left).unwrap().unwrap();
        assert!(left.contains([-3.5, 1.0, 1.0]));
        assert!(!left.contains([4.5, 1.0, 1.0]));
        let _ = std::fs::remove_dir_all(&d);
    }
}
