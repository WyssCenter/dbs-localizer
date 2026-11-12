// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The per-subject statistics file `sub-<id>_desc-stats.mat` (`specs/14` §3.1–§3.2): typed reader.

use std::path::{Path, PathBuf};

use fastwarp_matfile::mat5::Mat5Value;

use super::matio::{
    cell_texts, field_names, first_number, matrix_rows, numbers, read_variable, rows_to_mat,
    scalar_struct, struct_elements, text, text_cell,
};
use super::GroupError;

/// The variable the statistics file holds.
pub const EA_STATS_VARIABLE: &str = "ea_stats";

/// A K × A matrix as row vectors (empty when not computed).
pub type ContactMatrix = Vec<Vec<f64>>;

/// Atlas intersection numbers of one VTA mesh (`stimulation(k).vat(s, j)`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VatStats {
    pub amp: Vec<f64>,
    pub label: String,
    pub contact: f64,
    pub side: f64,
    pub volume: f64,
    pub atlas_intersection: Vec<f64>,
    pub n_atlas_intersection: Vec<f64>,
    pub n_within_atlas_intersection: Vec<f64>,
    pub atlas_volume: Vec<f64>,
}

/// Atlas intersection numbers of one E-field (`stimulation(k).efield(s, j)`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EfieldStats {
    pub volume: f64,
    pub atlas_intersection: Vec<f64>,
    pub n_atlas_intersection: Vec<f64>,
}

/// Parcel fibre counts of one side (`stimulation(k).ft(s)`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FtStats {
    pub fibercounts: Vec<f64>,
    pub nfibercounts: Vec<f64>,
    pub labels: Vec<String>,
}

/// The statistics of one stimulation label; sides are indexed 0 = right, 1 = left.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StimulationStats {
    pub label: String,
    pub vat: [Vec<VatStats>; 2],
    pub efield: [Vec<EfieldStats>; 2],
    pub ft: [Option<FtStats>; 2],
}

/// The fields of `ea_stats` that `specs/14` §3.2 lists; `electrodes` stays a raw value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EaStats {
    pub atlas_names: Vec<String>,
    pub atlas_types: Vec<f64>,
    pub electrodes: Option<Mat5Value>,
    pub patname: Vec<[String; 2]>,
    pub conmat: Vec<[ContactMatrix; 2]>,
    pub conmat_inside_hull: Vec<[ContactMatrix; 2]>,
    pub conmat_inside_vox: Vec<[ContactMatrix; 2]>,
    pub stimulation: Vec<StimulationStats>,
}

/// A statistics file read: the typed content and what could not be used.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EaStatsRead {
    pub stats: EaStats,
    pub notes: Vec<String>,
    pub unsupported: Vec<String>,
}

/// `<subject folder>/sub-<id>_desc-stats.mat`.
pub fn stats_path(subject_dir: &Path, subject: &str) -> PathBuf {
    subject_dir.join(format!("{subject}_desc-stats.mat"))
}

fn field<'a>(v: &'a Mat5Value, name: &str) -> Option<&'a Mat5Value> {
    v.field(name, 0)
}

fn nums(v: &Mat5Value, name: &str) -> Vec<f64> {
    field(v, name).map(numbers).unwrap_or_default()
}

fn num(v: &Mat5Value, name: &str) -> f64 {
    field(v, name).and_then(first_number).unwrap_or(f64::NAN)
}

fn side_pair<T>(items: Vec<T>, rows: usize) -> Vec<[T; 2]>
where
    T: Default,
{
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    (0..rows)
        .map(|e| {
            let right = slots.get_mut(e).and_then(Option::take).unwrap_or_default();
            let left = slots
                .get_mut(e + rows)
                .and_then(Option::take)
                .unwrap_or_default();
            [right, left]
        })
        .collect()
}

fn pair_cell(v: Option<&Mat5Value>) -> Vec<[ContactMatrix; 2]> {
    let Some(Mat5Value::Cell { dims, items }) = v else {
        return Vec::new();
    };
    let rows = dims.first().copied().unwrap_or(0);
    side_pair(items.iter().map(matrix_rows).collect(), rows)
}

fn vat_of(el: &Mat5Value) -> VatStats {
    VatStats {
        amp: nums(el, "amp"),
        label: field(el, "label")
            .and_then(text)
            .unwrap_or_default()
            .to_owned(),
        contact: num(el, "contact"),
        side: num(el, "side"),
        volume: num(el, "volume"),
        atlas_intersection: nums(el, "AtlasIntersection"),
        n_atlas_intersection: nums(el, "nAtlasIntersection"),
        n_within_atlas_intersection: nums(el, "nWithinAtlasIntersection"),
        atlas_volume: nums(el, "AtlasVolume"),
    }
}

fn efield_of(el: &Mat5Value) -> EfieldStats {
    EfieldStats {
        volume: num(el, "volume"),
        atlas_intersection: nums(el, "AtlasIntersection"),
        n_atlas_intersection: nums(el, "nAtlasIntersection"),
    }
}

fn per_side<T>(v: Option<&Mat5Value>, build: impl Fn(&Mat5Value) -> T) -> [Vec<T>; 2] {
    let mut out: [Vec<T>; 2] = [Vec::new(), Vec::new()];
    let Some(v) = v else { return out };
    let Mat5Value::Struct { dims, .. } = v else {
        return out;
    };
    let rows = dims.first().copied().unwrap_or(0).max(1);
    let two_rows = rows == 2;
    for (e, el) in struct_elements(v).iter().enumerate() {
        if is_blank(el) {
            continue;
        }
        let (s, _j) = if two_rows { (e % 2, e / 2) } else { (0, e) };
        out[s].push(build(el));
    }
    out
}

fn is_blank(el: &Mat5Value) -> bool {
    field_names(el)
        .iter()
        .all(|n| field(el, n).is_none_or(Mat5Value::is_empty_array))
}

fn ft_of(v: Option<&Mat5Value>) -> [Option<FtStats>; 2] {
    let mut out: [Option<FtStats>; 2] = [None, None];
    let Some(v) = v else { return out };
    for (s, el) in struct_elements(v).iter().enumerate().take(2) {
        if is_blank(el) {
            continue;
        }
        let first = |name: &str| {
            field(el, name)
                .and_then(|c| match c {
                    Mat5Value::Cell { items, .. } => items.first(),
                    other => Some(other),
                })
                .cloned()
        };
        out[s] = Some(FtStats {
            fibercounts: first("fibercounts")
                .map(|v| numbers(&v))
                .unwrap_or_default(),
            nfibercounts: first("nfibercounts")
                .map(|v| numbers(&v))
                .unwrap_or_default(),
            labels: first("labels")
                .and_then(|v| cell_texts(&v))
                .unwrap_or_default(),
        });
    }
    out
}

impl EaStats {
    /// Builds the typed statistics from the `ea_stats` value; legacy parts are noted in `notes`.
    pub fn from_mat(v: &Mat5Value, notes: &mut Vec<String>) -> Result<EaStats, GroupError> {
        if !matches!(v, Mat5Value::Struct { .. }) {
            return Err(GroupError::Mat("ea_stats is not a struct".to_owned()));
        }
        let mut stats = EaStats::default();
        if let Some(atlases) = field(v, "atlases") {
            stats.atlas_names = field(atlases, "names")
                .and_then(cell_texts)
                .unwrap_or_default();
            stats.atlas_types = nums(atlases, "types");
            if field_names(atlases).iter().any(|n| n == "rebuild") {
                notes.push(
                    "ea_stats.atlases holds the whole atlas table (old format); only names and types are kept"
                        .to_owned(),
                );
            }
        }
        stats.electrodes = field(v, "electrodes").cloned();
        if let Some(Mat5Value::Cell { dims, items }) = field(v, "patname") {
            let rows = dims.first().copied().unwrap_or(0);
            let names = items.iter().map(|i| text(i).unwrap_or("").to_owned());
            stats.patname = side_pair(names.collect(), rows);
        }
        stats.conmat = pair_cell(field(v, "conmat"));
        stats.conmat_inside_hull = pair_cell(field(v, "conmat_inside_hull"));
        stats.conmat_inside_vox = pair_cell(field(v, "conmat_inside_vox"));
        if let Some(stim) = field(v, "stimulation") {
            if field_names(stim).iter().any(|n| n == "label") {
                for el in struct_elements(stim) {
                    stats.stimulation.push(StimulationStats {
                        label: field(&el, "label")
                            .and_then(text)
                            .unwrap_or_default()
                            .to_owned(),
                        vat: per_side(field(&el, "vat"), vat_of),
                        efield: per_side(field(&el, "efield"), efield_of),
                        ft: ft_of(field(&el, "ft")),
                    });
                }
            } else if matches!(stim, Mat5Value::Struct { .. }) {
                notes.push(
                    "ea_stats.stimulation has no label field (legacy); the stimulation entries are discarded"
                        .to_owned(),
                );
            }
        }
        Ok(stats)
    }

    /// The entry of a stimulation label.
    pub fn stimulation_of(&self, label: &str) -> Option<&StimulationStats> {
        self.stimulation.iter().find(|s| s.label == label)
    }

    /// The `ea_stats` value in the layout `specs/14` §3.2 describes.
    pub fn to_mat(&self) -> Mat5Value {
        let mut fields: Vec<(&str, Mat5Value)> = Vec::new();
        let atlases = scalar_struct(vec![
            (
                "names",
                text_cell(vec![1, self.atlas_names.len()], &self.atlas_names),
            ),
            ("types", Mat5Value::row(&self.atlas_types)),
        ]);
        fields.push(("atlases", atlases));
        if let Some(e) = &self.electrodes {
            fields.push(("electrodes", e.clone()));
        }
        if !self.patname.is_empty() {
            let rows = self.patname.len();
            let mut items: Vec<String> = self.patname.iter().map(|p| p[0].clone()).collect();
            items.extend(self.patname.iter().map(|p| p[1].clone()));
            fields.push(("patname", text_cell(vec![rows, 2], &items)));
        }
        for (name, cells) in [
            ("conmat", &self.conmat),
            ("conmat_inside_hull", &self.conmat_inside_hull),
            ("conmat_inside_vox", &self.conmat_inside_vox),
        ] {
            if !cells.is_empty() {
                let rows = cells.len();
                let mut items: Vec<Mat5Value> = cells.iter().map(|c| rows_to_mat(&c[0])).collect();
                items.extend(cells.iter().map(|c| rows_to_mat(&c[1])));
                fields.push((
                    name,
                    Mat5Value::Cell {
                        dims: vec![rows, 2],
                        items,
                    },
                ));
            }
        }
        if !self.stimulation.is_empty() {
            let elements: Vec<Vec<Mat5Value>> =
                self.stimulation.iter().map(stim_elements).collect();
            fields.push((
                "stimulation",
                Mat5Value::Struct {
                    dims: vec![1, elements.len()],
                    fields: ["label", "vat", "efield", "ft"]
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                    elements,
                },
            ));
        }
        scalar_struct(fields)
    }
}

fn side_struct_array<T>(
    sides: &[Vec<T>; 2],
    names: &[&str],
    values: impl Fn(&T) -> Vec<Mat5Value>,
) -> Mat5Value {
    let j = sides[0].len().max(sides[1].len());
    if j == 0 {
        return Mat5Value::Empty;
    }
    let mut elements = Vec::with_capacity(2 * j);
    for jj in 0..j {
        for side in sides {
            elements.push(
                side.get(jj)
                    .map(&values)
                    .unwrap_or_else(|| vec![Mat5Value::Empty; names.len()]),
            );
        }
    }
    Mat5Value::Struct {
        dims: vec![2, j],
        fields: names.iter().map(|s| (*s).to_owned()).collect(),
        elements,
    }
}

fn stim_elements(s: &StimulationStats) -> Vec<Mat5Value> {
    let vat = side_struct_array(
        &s.vat,
        &[
            "amp",
            "label",
            "contact",
            "side",
            "volume",
            "AtlasIntersection",
            "nAtlasIntersection",
            "nWithinAtlasIntersection",
            "AtlasVolume",
        ],
        |v| {
            vec![
                Mat5Value::row(&v.amp),
                Mat5Value::char(&v.label),
                Mat5Value::scalar(v.contact),
                Mat5Value::scalar(v.side),
                Mat5Value::scalar(v.volume),
                Mat5Value::row(&v.atlas_intersection),
                Mat5Value::row(&v.n_atlas_intersection),
                Mat5Value::row(&v.n_within_atlas_intersection),
                Mat5Value::row(&v.atlas_volume),
            ]
        },
    );
    let efield = side_struct_array(
        &s.efield,
        &["volume", "AtlasIntersection", "nAtlasIntersection"],
        |e| {
            vec![
                Mat5Value::scalar(e.volume),
                Mat5Value::row(&e.atlas_intersection),
                Mat5Value::row(&e.n_atlas_intersection),
            ]
        },
    );
    let ft = if s.ft.iter().any(Option::is_some) {
        let elements: Vec<Vec<Mat5Value>> =
            s.ft.iter()
                .map(|f| match f {
                    Some(f) => vec![
                        Mat5Value::cell_row(vec![Mat5Value::row(&f.fibercounts)]),
                        Mat5Value::cell_row(vec![Mat5Value::row(&f.nfibercounts)]),
                        Mat5Value::cell_row(vec![text_cell(vec![1, f.labels.len()], &f.labels)]),
                    ],
                    None => vec![Mat5Value::Empty; 3],
                })
                .collect();
        Mat5Value::Struct {
            dims: vec![1, 2],
            fields: ["fibercounts", "nfibercounts", "labels"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            elements,
        }
    } else {
        Mat5Value::Empty
    };
    vec![Mat5Value::char(&s.label), vat, efield, ft]
}

/// Reads `sub-<id>_desc-stats.mat` (MAT v5, v7 or v7.3) into the typed statistics.
pub fn read_ea_stats(path: &Path) -> Result<EaStatsRead, GroupError> {
    let decoded = read_variable(path, EA_STATS_VARIABLE, &[])?.ok_or_else(|| {
        GroupError::Mat(format!(
            "{}: no variable {EA_STATS_VARIABLE}",
            path.display()
        ))
    })?;
    let mut notes = Vec::new();
    let stats = EaStats::from_mat(&decoded.value, &mut notes)?;
    Ok(EaStatsRead {
        stats,
        notes,
        unsupported: decoded.dropped,
    })
}
