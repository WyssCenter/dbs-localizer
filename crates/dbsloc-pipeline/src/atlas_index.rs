// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Lead-DBS `atlas_index.mat` (`specs/11` §3, §5.1, §5.2) and the default palette for atlas
//! folders without an index.

use std::path::Path;

use fastwarp_matfile::{MatError, MatFile, MatRef, MatValue};
use serde::{Deserialize, Serialize};

pub const ATLAS_INDEX_FILE: &str = "atlas_index.mat";
pub const DEFAULT_ALPHA: f64 = 0.7;

#[derive(Debug, thiserror::Error)]
pub enum AtlasIndexError {
    #[error(transparent)]
    Mat(#[from] MatError),
    #[error("atlas_index.mat: missing `{0}`")]
    Missing(&'static str),
    #[error("atlas_index.mat: `{field}`: {message}")]
    Invalid {
        field: &'static str,
        message: String,
    },
}

type Result<T> = std::result::Result<T, AtlasIndexError>;

fn invalid(field: &'static str, message: impl Into<String>) -> AtlasIndexError {
    AtlasIndexError::Invalid {
        field,
        message: message.into(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Right,
    Left,
    Midline,
}

impl Side {
    pub fn suffix(self) -> &'static str {
        match self {
            Side::Right => "right",
            Side::Left => "left",
            Side::Midline => "midline",
        }
    }

    /// Column of the `N×2` side cells: right / midline = column 1, left = column 2.
    pub fn column(self) -> usize {
        match self {
            Side::Right | Side::Midline => 0,
            Side::Left => 1,
        }
    }

    /// Sides a structure of atlas type `kind` produces (`specs/11` §2.1); unknown codes are
    /// treated like bilateral type 3.
    pub fn for_kind(kind: i64) -> &'static [Side] {
        match kind {
            1 => &[Side::Right],
            2 => &[Side::Left],
            5 => &[Side::Midline],
            _ => &[Side::Right, Side::Left],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StructureSide {
    pub side: Side,
    pub tag: String,
    pub color: Option<[f64; 3]>,
    pub alpha: f64,
    pub threshold: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AtlasStructure {
    /// 0-based position in `atlases.names`; preset indices refer to it.
    pub index: usize,
    pub file: String,
    pub name: String,
    pub kind: i64,
    pub tissue_type: Option<i64>,
    pub sides: Vec<StructureSide>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkippedEntry {
    pub index: usize,
    pub file: String,
    pub kind_text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AtlasPreset {
    pub label: String,
    pub show: Vec<usize>,
    pub hide: Vec<usize>,
    /// The preset's `default` text (`absolute`, `relative`, ...); `relative` when absent.
    pub mode: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LabelSet {
    pub name: String,
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThresholdRule {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AtlasIndex {
    pub version: Option<f64>,
    pub citation_name: Option<String>,
    pub citation_short: Option<String>,
    pub citation_long: Vec<String>,
    pub structures: Vec<AtlasStructure>,
    pub skipped: Vec<SkippedEntry>,
    pub presets: Vec<AtlasPreset>,
    pub default_preset: Option<usize>,
    pub labels: Vec<LabelSet>,
    pub threshold_rule: Option<ThresholdRule>,
}

/// Strips `.nii.gz`, `.nii`, `.trk.gz`, `.trk` or `.mat` (`specs/11` §2.2).
pub fn structure_name(file: &str) -> String {
    let lower = file.to_ascii_lowercase();
    for ext in [".nii.gz", ".nii", ".trk.gz", ".trk", ".mat"] {
        if lower.ends_with(ext) {
            return file[..file.len() - ext.len()].to_owned();
        }
    }
    file.to_owned()
}

struct Cell {
    rows: usize,
    cols: usize,
    items: Vec<MatRef>,
}

impl Cell {
    fn from(v: &MatValue) -> Option<Self> {
        let MatValue::Cell { dims, items } = v else {
            return None;
        };
        let rows = dims.first().copied().unwrap_or(0);
        let cols = dims.get(1..).map_or(1, |d| d.iter().product());
        Some(Cell {
            rows,
            cols,
            items: items.clone(),
        })
    }

    fn at(&self, row: usize, col: usize) -> Option<MatRef> {
        if row < self.rows && col < self.cols {
            self.items.get(row + col * self.rows).copied()
        } else {
            None
        }
    }
}

fn texts(f: &MatFile, v: &MatValue) -> Result<Vec<String>> {
    if let Some(s) = v.as_text() {
        return Ok(vec![s.to_owned()]);
    }
    let Some(items) = v.cell_items() else {
        return Ok(Vec::new());
    };
    items
        .iter()
        .map(|r| Ok(f.value(*r)?.as_text().unwrap_or_default().to_owned()))
        .collect()
}

fn numbers(v: Option<&MatValue>) -> Vec<f64> {
    v.and_then(MatValue::as_f64)
        .map(<[f64]>::to_vec)
        .unwrap_or_default()
}

fn indices(v: &MatValue, n: usize) -> Vec<usize> {
    numbers(Some(v))
        .into_iter()
        .filter(|x| x.is_finite() && x.round() >= 1.0)
        .map(|x| x.round() as usize - 1)
        .filter(|i| *i < n)
        .collect()
}

fn colormap(v: Option<&MatValue>) -> Option<Vec<[f64; 3]>> {
    let MatValue::Double { dims, data } = v? else {
        return None;
    };
    let k = *dims.first()?;
    if dims.get(1) != Some(&3) || data.len() != 3 * k || k == 0 {
        return None;
    }
    Some(
        (0..k)
            .map(|i| [data[i], data[k + i], data[2 * k + i]])
            .collect(),
    )
}

/// Rule 2 of `specs/11` §5.1: `colormap[round(colors[i])]`, the 1-based index clamped to 1..K.
pub fn palette_color(colormap: &[[f64; 3]], color_index: f64) -> Option<[f64; 3]> {
    if !color_index.is_finite() || colormap.is_empty() {
        return None;
    }
    let k = colormap.len();
    let i = (color_index.round().max(1.0) as usize).min(k);
    Some(colormap[i - 1])
}

impl AtlasIndex {
    pub fn read(path: &Path) -> Result<Self> {
        Self::from_mat(&MatFile::open(path)?)
    }

    /// Parses the `atlases` struct. Only `names`, `types`, `colormap`, `colors`, `pixdim`,
    /// `tissuetypes`, `threshold`, `version`, `citation`, `defaultset`, `presets`, `labels`,
    /// `labelnames` and the `color` / `alpha` / `threshold` / `Tag` fields of each `roi` element
    /// are read; the cached images and meshes (`roi.nii`, `roi.fv`, `roi.sfv`, `fv`, `XYZ`) are
    /// never dereferenced.
    pub fn from_mat(f: &MatFile) -> Result<Self> {
        let atl = f
            .get("atlases")?
            .ok_or(AtlasIndexError::Missing("atlases"))?;
        if !matches!(atl, MatValue::Struct { .. }) {
            return Err(invalid("atlases", "not a struct"));
        }
        let field = |name: &str| f.struct_field(&atl, name);
        let names = texts(
            f,
            &field("names")?.ok_or(AtlasIndexError::Missing("names"))?,
        )?;
        let n = names.len();
        let types = numbers(field("types")?.as_ref());
        if types.len() < n {
            return Err(invalid(
                "types",
                format!("{} value(s) for {n} name(s)", types.len()),
            ));
        }
        let cmap = colormap(field("colormap")?.as_ref());
        let colors = numbers(field("colors")?.as_ref());
        let tissue = numbers(field("tissuetypes")?.as_ref());
        let pixdim = field("pixdim")?.as_ref().and_then(Cell::from);
        let roi = field("roi")?.as_ref().and_then(Cell::from);

        let mut structures = Vec::new();
        let mut skipped = Vec::new();
        for (i, file) in names.iter().enumerate() {
            let kind = types[i].round() as i64;
            let kind_text = match &pixdim {
                Some(p) => pixdim_text(f, p, i)?,
                None => None,
            }
            .or_else(|| (kind == 10).then(|| "heatmap".to_owned()));
            if let Some(kind_text) = kind_text {
                skipped.push(SkippedEntry {
                    index: i,
                    file: file.clone(),
                    kind_text,
                });
                continue;
            }
            let name = structure_name(file);
            let fallback = cmap
                .as_deref()
                .and_then(|c| palette_color(c, colors.get(i).copied().unwrap_or(f64::NAN)));
            let sides = Side::for_kind(kind)
                .iter()
                .map(|side| {
                    let el = roi.as_ref().and_then(|r| r.at(i, side.column()));
                    roi_side(f, el, *side, &name, fallback)
                })
                .collect::<Result<Vec<_>>>()?;
            structures.push(AtlasStructure {
                index: i,
                file: file.clone(),
                name,
                kind,
                tissue_type: tissue
                    .get(i)
                    .filter(|t| t.is_finite())
                    .map(|t| t.round() as i64),
                sides,
            });
        }

        let (citation_name, citation_short, citation_long) = match field("citation")? {
            Some(c @ MatValue::Struct { .. }) => (
                f.struct_field(&c, "name")?
                    .and_then(|v| v.as_text().map(str::to_owned)),
                f.struct_field(&c, "short")?
                    .and_then(|v| v.as_text().map(str::to_owned)),
                match f.struct_field(&c, "long")? {
                    Some(v) => texts(f, &v)?,
                    None => Vec::new(),
                },
            ),
            _ => (None, None, Vec::new()),
        };

        let threshold_rule = match field("threshold")? {
            Some(t @ MatValue::Struct { .. }) => f
                .struct_field(&t, "type")?
                .and_then(|v| v.as_text().map(str::to_owned))
                .map(|kind| -> Result<ThresholdRule> {
                    Ok(ThresholdRule {
                        kind,
                        value: numbers(f.struct_field(&t, "value")?.as_ref()),
                    })
                })
                .transpose()?,
            _ => None,
        };

        let presets = match field("presets")? {
            Some(p @ MatValue::Struct { .. }) => read_presets(f, &p, n)?,
            _ => Vec::new(),
        };
        let default_preset = field("defaultset")?
            .and_then(|v| v.scalar())
            .filter(|d| d.is_finite() && d.round() >= 1.0)
            .map(|d| d.round() as usize - 1)
            .filter(|d| *d < presets.len());

        let mut labels = Vec::new();
        if let Some(sets) = field("labels")? {
            let set_names = match field("labelnames")? {
                Some(v) => texts(f, &v)?,
                None => Vec::new(),
            };
            for (k, r) in sets.cell_items().unwrap_or_default().iter().enumerate() {
                labels.push(LabelSet {
                    name: set_names
                        .get(k)
                        .cloned()
                        .unwrap_or_else(|| format!("Labels {}", k + 1)),
                    labels: texts(f, &f.value(*r)?)?,
                });
            }
        }

        Ok(AtlasIndex {
            version: field("version")?.and_then(|v| v.scalar()),
            citation_name,
            citation_short,
            citation_long,
            structures,
            skipped,
            presets,
            default_preset,
            labels,
            threshold_rule,
        })
    }

    /// Initial visibility per entry of `structures` (`specs/11` §5.2): with presets and a
    /// default preset, visible iff listed in its `show`; otherwise everything is visible.
    pub fn initially_visible(&self) -> Vec<bool> {
        match self.default_preset.and_then(|d| self.presets.get(d)) {
            Some(p) => self
                .structures
                .iter()
                .map(|s| p.show.contains(&s.index))
                .collect(),
            None => vec![true; self.structures.len()],
        }
    }
}

fn pixdim_text(f: &MatFile, p: &Cell, row: usize) -> Result<Option<String>> {
    for col in 0..p.cols {
        if let Some(r) = p.at(row, col) {
            if let Some(t) = f.value(r)?.as_text().filter(|t| !t.is_empty()) {
                return Ok(Some(t.to_owned()));
            }
        }
    }
    Ok(None)
}

fn roi_side(
    f: &MatFile,
    el: Option<MatRef>,
    side: Side,
    name: &str,
    fallback: Option<[f64; 3]>,
) -> Result<StructureSide> {
    let default_tag = format!("{name}_{}", side.suffix());
    let roi = match el {
        Some(r) => Some(f.value(r)?).filter(|v| matches!(v, MatValue::Struct { .. })),
        None => None,
    };
    let Some(roi) = roi else {
        return Ok(StructureSide {
            side,
            tag: default_tag,
            color: fallback,
            alpha: DEFAULT_ALPHA,
            threshold: None,
        });
    };
    let color = match f.struct_field(&roi, "color")? {
        Some(MatValue::Double { data, .. }) if data.len() == 3 => Some([data[0], data[1], data[2]]),
        _ => None,
    }
    .or(fallback);
    let finite = |v: Option<MatValue>| v.and_then(|v| v.scalar()).filter(|x| x.is_finite());
    let alpha = finite(f.struct_field(&roi, "alpha")?).unwrap_or(DEFAULT_ALPHA);
    let threshold = finite(f.struct_field(&roi, "threshold")?);
    let tag = f
        .struct_field(&roi, "Tag")?
        .and_then(|v| v.as_text().map(|t| t.trim().to_owned()))
        .filter(|t| !t.is_empty())
        .unwrap_or(default_tag);
    Ok(StructureSide {
        side,
        tag,
        color,
        alpha,
        threshold,
    })
}

fn read_presets(f: &MatFile, p: &MatValue, n: usize) -> Result<Vec<AtlasPreset>> {
    let get = |name: &str| f.struct_field(p, name);
    let (label, show, hide, mode) = (get("label")?, get("show")?, get("hide")?, get("default")?);
    let element = |v: &Option<MatValue>, k: usize| -> Result<Option<MatValue>> {
        match v {
            Some(MatValue::Cell { items, .. }) => match items.get(k) {
                Some(r) => f.value(*r).map(Some).map_err(Into::into),
                None => Ok(None),
            },
            Some(other) if k == 0 => Ok(Some(other.clone())),
            _ => Ok(None),
        }
    };
    let count = match &label {
        Some(MatValue::Cell { items, .. }) => items.len(),
        Some(_) => 1,
        None => return Ok(Vec::new()),
    };
    (0..count)
        .map(|k| {
            Ok(AtlasPreset {
                label: element(&label, k)?
                    .and_then(|v| v.as_text().map(str::to_owned))
                    .unwrap_or_default(),
                show: element(&show, k)?.map_or_else(Vec::new, |v| indices(&v, n)),
                hide: element(&hide, k)?.map_or_else(Vec::new, |v| indices(&v, n)),
                mode: element(&mode, k)?
                    .and_then(|v| v.as_text().map(str::to_owned))
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| "relative".to_owned()),
            })
        })
        .collect()
}

const BU_OR_R_14: [[f64; 3]; 14] = [
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

fn sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

fn pchip_edge(h0: f64, h1: f64, m0: f64, m1: f64) -> f64 {
    let d = ((2.0 * h0 + h1) * m0 - h0 * m1) / (h0 + h1);
    if sign(d) != sign(m0) {
        0.0
    } else if sign(m0) != sign(m1) && d.abs() > 3.0 * m0.abs() {
        3.0 * m0
    } else {
        d
    }
}

/// Fritsch–Carlson PCHIP slopes on equally spaced knots with SciPy's end conditions.
fn pchip_slopes(h: f64, y: &[f64]) -> Vec<f64> {
    let n = y.len();
    let m: Vec<f64> = y.windows(2).map(|w| (w[1] - w[0]) / h).collect();
    let mut d = vec![0.0; n];
    for k in 1..n - 1 {
        let (a, b) = (m[k - 1], m[k]);
        if sign(a) == sign(b) && a != 0.0 && b != 0.0 {
            let (w1, w2) = (3.0 * h, 3.0 * h);
            d[k] = (w1 + w2) / (w1 / a + w2 / b);
        }
    }
    d[0] = pchip_edge(h, h, m[0], m[1]);
    d[n - 1] = pchip_edge(h, h, m[n - 2], m[n - 3]);
    d
}

fn pchip_eval(h: f64, y: &[f64], d: &[f64], t: f64) -> f64 {
    let k = ((t / h).floor().max(0.0) as usize).min(y.len() - 2);
    let u = (t - k as f64 * h) / h;
    let (u2, u3) = (u * u, u * u * u);
    (2.0 * u3 - 3.0 * u2 + 1.0) * y[k]
        + (u3 - 2.0 * u2 + u) * h * d[k]
        + (-2.0 * u3 + 3.0 * u2) * y[k + 1]
        + (u3 - u2) * h * d[k + 1]
}

/// Default structure colours for an atlas folder without `atlas_index.mat` (`specs/11` §5.1
/// rule 3): BuOrR_14 resampled to `n` entries with PCHIP, clipped to [0, 1].
pub fn default_palette(n: usize) -> Vec<[f32; 3]> {
    let h = 1.0 / (BU_OR_R_14.len() - 1) as f64;
    let channels: Vec<(Vec<f64>, Vec<f64>)> = (0..3)
        .map(|c| {
            let y: Vec<f64> = BU_OR_R_14.iter().map(|s| s[c]).collect();
            let d = pchip_slopes(h, &y);
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
            let mut rgb = [0f32; 3];
            for (c, (y, d)) in channels.iter().enumerate() {
                rgb[c] = pchip_eval(h, y, d, t).clamp(0.0, 1.0) as f32;
            }
            rgb
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f64; 3], tol: f64) -> bool {
        a.iter()
            .zip(b)
            .all(|(x, y)| (f64::from(*x) - y).abs() <= tol)
    }

    #[test]
    fn atlas_default_palette_matches_the_check_values() {
        let p4 = default_palette(4);
        for (got, want) in p4.iter().zip([
            [0.03, 0.353, 1.0],
            [0.8072, 0.8711, 1.0],
            [1.0, 1.0, 0.1333],
            [1.0, 0.0, 0.0],
        ]) {
            assert!(close(*got, want, 1e-3), "{got:?} vs {want:?}");
        }
        let p15 = default_palette(15);
        assert!(close(p15[0], [0.03, 0.353, 1.0], 1e-3));
        assert!(close(p15[7], [0.9902, 0.9935, 0.925], 1e-3), "{:?}", p15[7]);
        assert!(close(p15[14], [1.0, 0.0, 0.0], 1e-3));
        assert!(close(default_palette(1)[0], [1.0, 0.0, 0.0], 1e-6));
        assert!(default_palette(0).is_empty());
        let p14 = default_palette(14);
        for (got, want) in p14.iter().zip(BU_OR_R_14) {
            assert!(close(*got, want, 1e-6));
        }
    }

    #[test]
    fn atlas_palette_index_is_rounded_and_clamped() {
        let cmap = [[0.0, 0.0, 0.0], [0.5, 0.5, 0.5], [1.0, 1.0, 1.0]];
        assert_eq!(palette_color(&cmap, 1.6), Some(cmap[1]));
        assert_eq!(palette_color(&cmap, 0.2), Some(cmap[0]));
        assert_eq!(palette_color(&cmap, 9.0), Some(cmap[2]));
        assert_eq!(palette_color(&cmap, f64::NAN), None);
        assert_eq!(palette_color(&[], 1.0), None);
    }

    #[test]
    fn atlas_structure_names_drop_the_extension() {
        assert_eq!(structure_name("STN_motor.nii.gz"), "STN_motor");
        assert_eq!(structure_name("GPi.NII"), "GPi");
        assert_eq!(structure_name("tract.trk"), "tract");
        assert_eq!(structure_name("plain"), "plain");
    }

    fn distal() -> Option<AtlasIndex> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../resources/atlases/leaddbs/DISTAL Minimal (Ewert 2017)")
            .join(ATLAS_INDEX_FILE);
        if !p.is_file() {
            eprintln!("skipped: {} is absent", p.display());
            return None;
        }
        Some(AtlasIndex::read(&p).unwrap())
    }

    const DISTAL_FILES: [&str; 15] = [
        "STN",
        "STN_motor",
        "STN_associative",
        "STN_limbic",
        "GPi",
        "GPi_sensory",
        "GPi_primarymotor",
        "GPi_premotor",
        "GPi_sensorimotor",
        "GPi_postparietal",
        "GPi_occipital",
        "GPi_temporal",
        "GPi_prefrontal",
        "RN",
        "GPe",
    ];

    #[test]
    fn atlas_index_distal_minimal_structures() {
        let Some(idx) = distal() else {
            return;
        };
        assert_eq!(idx.version, Some(2.1));
        assert!(idx.skipped.is_empty());
        assert_eq!(idx.structures.len(), 15);
        for (i, (s, want)) in idx.structures.iter().zip(DISTAL_FILES).enumerate() {
            assert_eq!(s.index, i);
            assert_eq!(s.name, want);
            assert_eq!(s.file, format!("{want}.nii.gz"));
            assert_eq!(s.kind, 3);
            assert_eq!(s.tissue_type, Some(1));
            assert_eq!(s.sides.len(), 2);
            assert_eq!(s.sides[0].side, Side::Right);
            assert_eq!(s.sides[1].side, Side::Left);
            for side in &s.sides {
                assert_eq!(side.alpha, 0.7);
                assert_eq!(side.tag, format!("{want}_{}", side.side.suffix()));
            }
        }
        let rule = idx.threshold_rule.as_ref().unwrap();
        assert_eq!(rule.kind, "relative_intensity");
        assert_eq!(rule.value, vec![0.5]);
        let stn = &idx.structures[0];
        assert_eq!(stn.sides[0].threshold, Some(0.4));
        assert_eq!(stn.sides[1].threshold, Some(0.4));
        let motor = &idx.structures[1].sides;
        assert!((motor[0].threshold.unwrap() - 0.49966).abs() < 1e-5);
        assert!((motor[1].threshold.unwrap() - 0.49950).abs() < 1e-5);
    }

    #[test]
    fn atlas_index_distal_minimal_colours() {
        let Some(idx) = distal() else {
            return;
        };
        let by_name = |n: &str| idx.structures.iter().find(|s| s.name == n).unwrap();
        for (name, want) in [
            ("STN", [1.0, 0.5, 0.0]),
            ("GPi", [0.5, 1.0, 0.5]),
            ("RN", [0.5, 0.0, 0.0]),
            ("GPe", [0.0, 0.5, 1.0]),
            ("STN_associative", [0.0, 0.75, 1.0]),
            ("STN_limbic", [0.9763, 0.9831, 0.0538]),
            ("GPi_sensory", [0.8, 0.2, 0.7]),
            ("GPi_prefrontal", [0.1, 0.2, 1.0]),
        ] {
            for side in &by_name(name).sides {
                let got = side.color.unwrap();
                assert!(
                    got.iter().zip(want).all(|(a, b)| (a - b).abs() <= 1e-4),
                    "{name} {:?}: {got:?} vs {want:?}",
                    side.side
                );
            }
        }
    }

    #[test]
    fn atlas_index_distal_minimal_presets_and_visibility() {
        let Some(idx) = distal() else {
            return;
        };
        let want: [(&str, &[usize], &[usize]); 6] = [
            (
                "Primary DBS targets",
                &[0, 4, 13, 14],
                &[1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12],
            ),
            ("STN: Solid", &[0], &[1, 2, 3]),
            ("STN: Subzones", &[1, 2, 3], &[0]),
            ("GPi: Solid", &[4], &[5, 6, 7, 8, 9, 10, 11, 12]),
            (
                "GPi: Sensorimotor functional zone",
                &[4, 8],
                &[5, 6, 7, 9, 10, 11, 12],
            ),
            (
                "GPi: Detailed functional zones",
                &[5, 6, 7, 9, 10, 11, 12],
                &[4, 8],
            ),
        ];
        assert_eq!(idx.presets.len(), 6);
        for (p, (label, show, hide)) in idx.presets.iter().zip(want) {
            assert_eq!(p.label, label);
            let (mut s, mut h) = (p.show.clone(), p.hide.clone());
            s.sort_unstable();
            h.sort_unstable();
            assert_eq!(s, show, "{label}");
            assert_eq!(h, hide, "{label}");
        }
        assert_eq!(idx.presets[0].mode, "absolute");
        assert_eq!(idx.presets[1].mode, "relative");
        assert_eq!(idx.default_preset, Some(0));
        let visible: Vec<&str> = idx
            .structures
            .iter()
            .zip(idx.initially_visible())
            .filter(|(_, v)| *v)
            .map(|(s, _)| s.name.as_str())
            .collect();
        assert_eq!(visible, ["STN", "GPi", "RN", "GPe"]);
        let shown_meshes: usize = idx
            .structures
            .iter()
            .zip(idx.initially_visible())
            .filter(|(_, v)| *v)
            .map(|(s, _)| s.sides.len())
            .sum();
        assert_eq!(shown_meshes, 8);
    }

    #[test]
    fn atlas_index_distal_minimal_citation_and_labels() {
        let Some(idx) = distal() else {
            return;
        };
        assert_eq!(
            idx.citation_name.as_deref(),
            Some("DISTAL Minimal Atlas (Ewert 2017)")
        );
        assert_eq!(idx.citation_short.as_deref(), Some("Ewert et al. 2017"));
        assert_eq!(idx.citation_long.len(), 2);
        assert!(idx.citation_long[0].contains("Ewert"));
        assert!(idx.citation_long[1].contains("Chakravarty"));
        let names: Vec<&str> = idx.labels.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Abbreviations", "Names"]);
        assert_eq!(idx.labels[0].labels.len(), 15);
        assert_eq!(idx.labels[0].labels[0], "STN");
        assert_eq!(idx.labels[1].labels[0], "Subthalamic Nucleus");
        assert_eq!(idx.labels[1].labels[13], "Red nucleus");
    }

    #[test]
    fn atlas_index_round_trips_through_json() {
        let Some(idx) = distal() else {
            return;
        };
        let v = serde_json::to_value(&idx).unwrap();
        assert_eq!(v["structures"][0]["sides"][0]["side"], "right");
        assert_eq!(v["structures"][0]["sides"][0]["tag"], "STN_right");
        assert_eq!(v["presets"][0]["show"], serde_json::json!([0, 4, 13, 14]));
        assert_eq!(v["threshold_rule"]["type"], "relative_intensity");
        let back: AtlasIndex = serde_json::from_value(v).unwrap();
        assert_eq!(back, idx);
    }

    #[test]
    fn atlas_index_parse_reads_only_a_small_part_of_the_file() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../resources/atlases/leaddbs/DISTAL Minimal (Ewert 2017)")
            .join(ATLAS_INDEX_FILE);
        if !p.is_file() {
            return;
        }
        let f = MatFile::open(&p).unwrap();
        AtlasIndex::from_mat(&f).unwrap();
        let file_len = std::fs::metadata(&p).unwrap().len();
        assert!(
            f.bytes_loaded() < 4 << 20 && f.bytes_loaded() * 4 < file_len,
            "{} of {file_len} bytes read",
            f.bytes_loaded()
        );
    }
}
