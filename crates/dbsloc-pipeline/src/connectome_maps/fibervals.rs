// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Per-fibre values for fibre filtering, the feature-matrix contract and fibre subset files
//! (`specs/15` §7).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use dbsloc_connectome::FibreSet;
use fastwarp_matfile::mat5::write_mat5_file;
use fastwarp_matfile::Mat5Value;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::nii::{apply, invert_affine, NiiVolume};
use super::source::FibreSource;
use super::{check_cancel, MapError, MapResult};

/// Threshold of a binary VTA input (`specs/15` §7.1).
pub const BINARY_THRESHOLD: f64 = 0.5;

/// Default E-field threshold in V/m (`specs/15` §7.1, `14` §8.2).
pub const DEFAULT_THRESHOLD_VPM: f64 = 200.0;

const SUBSET_LIMIT_BYTES: usize = 1_500_000_000;

/// The metrics of `specs/15` §7.2 for every connected fibre of one image.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FibreValues {
    pub total_fibers: usize,
    pub threshold: f64,
    /// Ascending 0-based fibre ids with at least one suprathreshold voxel.
    pub fiber_index: Vec<u32>,
    pub bin: Vec<u8>,
    pub sum: Vec<f32>,
    pub mean: Vec<f32>,
    pub peak: Vec<f32>,
    pub peak5: Vec<f32>,
}

struct Stats {
    sum: f64,
    mean: f64,
    peak: f64,
    peak5: f64,
}

fn stats_of(values: &mut [f64]) -> Stats {
    let n = values.len();
    let sum: f64 = values.iter().sum();
    values.sort_by(|a, b| b.total_cmp(a));
    let top = ((0.05 * n as f64).ceil() as usize).clamp(1, n);
    let peak5 = values[..top].iter().sum::<f64>() / top as f64;
    Stats {
        sum,
        mean: sum / n as f64,
        peak: values[0],
        peak5,
    }
}

/// Computes the per-fibre values of image `e` (`specs/15` §7.2): voxels with `|E| > threshold`
/// are suprathreshold; only fibre points inside their voxel-centre bounding box count; a fibre
/// with a kept point outside the image grid is dropped; statistics use the signed values of the
/// distinct suprathreshold voxels the fibre visits.
pub fn fibre_values(
    set: &FibreSet,
    e: &NiiVolume,
    threshold: f64,
    cancel: &AtomicBool,
) -> MapResult<FibreValues> {
    fibre_values_in(set, None, set.n_fibres(), e, threshold, cancel)
}

/// The box of the voxel centres with `|E| > threshold`, `None` when there is none.
pub fn above_threshold_box(e: &NiiVolume, threshold: f64) -> Option<([f64; 3], [f64; 3])> {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut any = false;
    for (n, v) in e.data.iter().enumerate() {
        if v.is_finite() && f64::from(v.abs()) > threshold {
            let c = e.centre(n);
            for a in 0..3 {
                lo[a] = lo[a].min(c[a]);
                hi[a] = hi[a].max(c[a]);
            }
            any = true;
        }
    }
    any.then_some((lo, hi))
}

/// [`fibre_values`] over a source that can answer "the fibres in this box": only the fibres with
/// a point inside the voxel-centre box can have a kept point, so only those are read.
pub fn fibre_values_from<S: FibreSource + ?Sized>(
    src: &S,
    e: &NiiVolume,
    threshold: f64,
    cancel: &AtomicBool,
) -> MapResult<FibreValues> {
    let k = src.n_fibres();
    let Some((lo, hi)) = above_threshold_box(e, threshold) else {
        return Ok(FibreValues {
            total_fibers: k,
            threshold,
            ..FibreValues::default()
        });
    };
    let region = src.region(lo, hi)?;
    fibre_values_in(
        &region.set,
        region.source_ids.as_deref(),
        k,
        e,
        threshold,
        cancel,
    )
}

fn fibre_values_in(
    set: &FibreSet,
    source_ids: Option<&[u32]>,
    total: usize,
    e: &NiiVolume,
    threshold: f64,
    cancel: &AtomicBool,
) -> MapResult<FibreValues> {
    let k = set.n_fibres();
    let mut out = FibreValues {
        total_fibers: total,
        threshold,
        ..FibreValues::default()
    };
    let above = |v: f32| v.is_finite() && f64::from(v.abs()) > threshold;
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut any = false;
    for (n, v) in e.data.iter().enumerate() {
        if above(*v) {
            let c = e.centre(n);
            for a in 0..3 {
                lo[a] = lo[a].min(c[a]);
                hi[a] = hi[a].max(c[a]);
            }
            any = true;
        }
    }
    if !any || k == 0 {
        return Ok(out);
    }
    let inv = invert_affine(&e.affine)
        .ok_or_else(|| MapError::Format("E-field affine is singular".into()))?;
    let dims = e.dims;
    let rows: Vec<Option<(u32, Stats)>> = (0..k)
        .into_par_iter()
        .map_init(
            || (Vec::<u32>::new(), Vec::<f64>::new()),
            |(voxels, values), f| {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                voxels.clear();
                for p in set.fibre(f) {
                    let w = [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])];
                    if (0..3).any(|a| w[a] < lo[a] || w[a] > hi[a]) {
                        continue;
                    }
                    let u = apply(&inv, w).map(|c| (c + 0.5).floor());
                    if u.iter()
                        .zip(dims)
                        .any(|(c, d)| !(*c >= 0.0 && *c < d as f64))
                    {
                        voxels.clear();
                        return None;
                    }
                    voxels.push(
                        ((u[2] as usize * dims[1] + u[1] as usize) * dims[0] + u[0] as usize)
                            as u32,
                    );
                }
                voxels.sort_unstable();
                voxels.dedup();
                values.clear();
                values.extend(
                    voxels
                        .iter()
                        .map(|v| e.data[*v as usize])
                        .filter(|v| above(*v))
                        .map(f64::from),
                );
                if values.is_empty() {
                    None
                } else {
                    Some((source_ids.map_or(f as u32, |ids| ids[f]), stats_of(values)))
                }
            },
        )
        .collect();
    check_cancel(cancel)?;
    for row in rows.into_iter().flatten() {
        out.fiber_index.push(row.0);
        out.bin.push(1);
        out.sum.push(row.1.sum as f32);
        out.mean.push(row.1.mean as f32);
        out.peak.push(row.1.peak as f32);
        out.peak5.push(row.1.peak5 as f32);
    }
    Ok(out)
}

/// The metric matrices of `specs/15` §7.3 restricted to the connected rows: row `r` is fibre
/// `connected[r]`, column `c` is input `c`; matrices are row-major `connected.len() x n_inputs`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FeatureMatrix {
    pub total_fibers: usize,
    pub n_inputs: usize,
    pub connected: Vec<u32>,
    pub bin: Vec<f32>,
    pub sum: Vec<f32>,
    pub mean: Vec<f32>,
    pub peak: Vec<f32>,
    pub peak5: Vec<f32>,
}

impl FeatureMatrix {
    /// Stacks one [`FibreValues`] per input (all of the same connectome).
    pub fn from_columns(columns: &[FibreValues]) -> MapResult<FeatureMatrix> {
        let total = columns.first().map_or(0, |c| c.total_fibers);
        if columns.iter().any(|c| c.total_fibers != total) {
            return Err(MapError::Usage(
                "the inputs come from different connectomes (fibre counts differ)".into(),
            ));
        }
        let mut connected: Vec<u32> = columns
            .iter()
            .flat_map(|c| {
                c.fiber_index
                    .iter()
                    .zip(&c.bin)
                    .filter(|(_, b)| **b == 1)
                    .map(|(f, _)| *f)
            })
            .collect();
        connected.sort_unstable();
        connected.dedup();
        let n = columns.len();
        let rows = connected.len();
        let mut m = FeatureMatrix {
            total_fibers: total,
            n_inputs: n,
            bin: vec![0.0; rows * n],
            sum: vec![0.0; rows * n],
            mean: vec![0.0; rows * n],
            peak: vec![0.0; rows * n],
            peak5: vec![0.0; rows * n],
            connected,
        };
        for (c, col) in columns.iter().enumerate() {
            for (i, f) in col.fiber_index.iter().enumerate() {
                if let Ok(r) = m.connected.binary_search(f) {
                    let at = r * n + c;
                    m.bin[at] = f32::from(col.bin[i]);
                    m.sum[at] = col.sum[i];
                    m.mean[at] = col.mean[i];
                    m.peak[at] = col.peak[i];
                    m.peak5[at] = col.peak5[i];
                }
            }
        }
        Ok(m)
    }

    /// The value of a metric (`"bin"`, `"sum"`, `"mean"`, `"peak"`, `"peak5"`) for fibre `f` of
    /// input `c`; 0 for a fibre outside the connected set.
    pub fn value(&self, metric: &str, f: u32, c: usize) -> Option<f32> {
        let data = match metric {
            "bin" => &self.bin,
            "sum" => &self.sum,
            "mean" => &self.mean,
            "peak" => &self.peak,
            "peak5" => &self.peak5,
            _ => return None,
        };
        if c >= self.n_inputs {
            return None;
        }
        Some(
            self.connected
                .binary_search(&f)
                .map_or(0.0, |r| data[r * self.n_inputs + c]),
        )
    }
}

/// Facts stored in a `fibervals.mat` next to the values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FibervalsMeta {
    pub connectome_tag: String,
    pub connectome_digest: String,
    pub efield_file: String,
    pub efield_sha256: String,
}

fn col(data: Vec<f64>) -> Mat5Value {
    if data.is_empty() {
        Mat5Value::Empty
    } else {
        Mat5Value::Double {
            dims: vec![data.len(), 1],
            data,
        }
    }
}

fn mat_err(path: &Path, e: impl std::fmt::Display) -> MapError {
    MapError::Io(format!("{}: {e}", path.display()))
}

/// Writes the `…_fibervals.mat` of `specs/15` §9.2 (MAT v7, numbers as doubles) at exactly `path`.
pub fn write_fibervals_mat(path: &Path, v: &FibreValues, meta: &FibervalsMeta) -> MapResult<()> {
    let widen = |x: &[f32]| x.iter().map(|a| f64::from(*a)).collect::<Vec<_>>();
    let vars = vec![
        (
            "fiber_index".to_owned(),
            col(v.fiber_index.iter().map(|f| f64::from(*f) + 1.0).collect()),
        ),
        (
            "bin".to_owned(),
            col(v.bin.iter().map(|b| f64::from(*b)).collect()),
        ),
        ("sum".to_owned(), col(widen(&v.sum))),
        ("mean".to_owned(), col(widen(&v.mean))),
        ("peak".to_owned(), col(widen(&v.peak))),
        ("peak5".to_owned(), col(widen(&v.peak5))),
        (
            "total_fibers".to_owned(),
            Mat5Value::scalar(v.total_fibers as f64),
        ),
        ("threshold_vpm".to_owned(), Mat5Value::scalar(v.threshold)),
        (
            "connectome_tag".to_owned(),
            Mat5Value::char(&meta.connectome_tag),
        ),
        (
            "connectome_digest".to_owned(),
            Mat5Value::char(&meta.connectome_digest),
        ),
        ("efield_file".to_owned(), Mat5Value::char(&meta.efield_file)),
        (
            "efield_sha256".to_owned(),
            Mat5Value::char(&meta.efield_sha256),
        ),
    ];
    write_mat5_file(path, &vars, true).map_err(|e| mat_err(path, e))
}

/// Writes the fibres `ids` of `set` as a `specs/15` §7.4 subset file (`ea_fibformat` 1.1, MAT v7,
/// numbers as doubles) at exactly `path`: fibre numbers renumbered 1..K', `vals` coloured values,
/// `source_fiber_index` (1-based source numbers) and `source_connectome`.
pub fn write_subset_mat(
    path: &Path,
    set: &FibreSet,
    ids: &[u32],
    vals: &[f32],
    source_tag: &str,
) -> MapResult<()> {
    write_subset_mat_with_source(path, set, ids, ids, vals, source_tag)
}

/// [`write_subset_mat`] for a set that holds only a subset of the connectome: `ids` index `set`,
/// `source_ids` are the 0-based fibre numbers in the source connectome.
pub fn write_subset_mat_with_source(
    path: &Path,
    set: &FibreSet,
    ids: &[u32],
    source_ids: &[u32],
    vals: &[f32],
    source_tag: &str,
) -> MapResult<()> {
    if ids.len() != source_ids.len() {
        return Err(MapError::Usage(
            "one source id per subset fibre is needed".into(),
        ));
    }
    if ids.len() != vals.len() {
        return Err(MapError::Usage(
            "one value per subset fibre is needed".into(),
        ));
    }
    let points: usize = ids.iter().map(|f| set.fibre(*f as usize).len()).sum();
    let bytes = points.saturating_mul(4).saturating_mul(8);
    if bytes > SUBSET_LIMIT_BYTES {
        return Err(MapError::SubsetTooLarge(format!(
            "{points} points exceed the 1.5 GB limit of a MAT v7 variable"
        )));
    }
    let mut fibers = vec![0f64; points * 4];
    let mut at = 0usize;
    let mut idx = Vec::with_capacity(ids.len());
    for (n, f) in ids.iter().enumerate() {
        let pts = set.fibre(*f as usize);
        idx.push(pts.len() as f64);
        for p in pts {
            fibers[at] = f64::from(p[0]);
            fibers[points + at] = f64::from(p[1]);
            fibers[2 * points + at] = f64::from(p[2]);
            fibers[3 * points + at] = (n + 1) as f64;
            at += 1;
        }
    }
    let vars = vec![
        (
            "fibers".to_owned(),
            Mat5Value::Double {
                dims: vec![points, 4],
                data: fibers,
            },
        ),
        ("idx".to_owned(), col(idx)),
        ("ea_fibformat".to_owned(), Mat5Value::char("1.1")),
        ("fourindex".to_owned(), Mat5Value::scalar(1.0)),
        ("voxmm".to_owned(), Mat5Value::char("mm")),
        (
            "vals".to_owned(),
            col(vals.iter().map(|v| f64::from(*v)).collect()),
        ),
        (
            "source_fiber_index".to_owned(),
            col(source_ids.iter().map(|f| f64::from(*f) + 1.0).collect()),
        ),
        ("source_connectome".to_owned(), Mat5Value::char(source_tag)),
    ];
    write_mat5_file(path, &vars, true).map_err(|e| mat_err(path, e))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::connectome_maps::nii::diagonal_affine;
    use fastwarp_matfile::mat5::read_mat5_file;

    fn field() -> NiiVolume {
        let mut e = NiiVolume::zeros([6, 3, 3], diagonal_affine([1.0; 3], [0.0; 3]));
        for (i, v) in [(1, 300.0), (2, 500.0), (3, 100.0)] {
            let n = e.index(i, 1, 1);
            e.data[n] = v;
        }
        e
    }

    fn fibre(y: f32, x0: f32, x1: f32) -> Vec<[f32; 3]> {
        let mut out = Vec::new();
        let mut x = x0;
        while x <= x1 + 1e-6 {
            out.push([x, y, 1.0]);
            x += 0.5;
        }
        out
    }

    #[test]
    fn linear_field_values_match_hand_computation() {
        let set = FibreSet::from_fibres(&[
            fibre(1.0, 0.0, 4.0),
            fibre(2.0, 0.0, 4.0),
            fibre(1.0, 0.0, 3.0),
        ])
        .unwrap();
        let cancel = AtomicBool::new(false);
        let v = fibre_values(&set, &field(), 200.0, &cancel).unwrap();
        assert_eq!(v.total_fibers, 3);
        assert_eq!(v.fiber_index, vec![0, 2]);
        assert_eq!(v.bin, vec![1, 1]);
        assert_eq!(v.sum, vec![800.0, 800.0]);
        assert_eq!(v.mean, vec![400.0, 400.0]);
        assert_eq!(v.peak, vec![500.0, 500.0]);
        assert_eq!(v.peak5, vec![500.0, 500.0]);
    }

    #[test]
    fn points_beyond_the_suprathreshold_box_are_not_counted_and_signs_are_kept() {
        let mut e = field();
        let n = e.index(2, 1, 1);
        e.data[n] = -500.0;
        let set = FibreSet::from_fibres(&[fibre(1.0, 0.0, 5.0)]).unwrap();
        let cancel = AtomicBool::new(false);
        let v = fibre_values(&set, &e, 200.0, &cancel).unwrap();
        assert_eq!(v.sum, vec![-200.0]);
        assert_eq!(v.peak, vec![300.0]);
        assert_eq!(v.mean, vec![-100.0]);
        let tight = FibreSet::from_fibres(&[vec![[4.9, 1.0, 1.0]]]).unwrap();
        assert!(fibre_values(&tight, &field(), 200.0, &cancel)
            .unwrap()
            .fiber_index
            .is_empty());
    }

    #[test]
    fn binary_mode_counts_voxels_and_empty_threshold_gives_nothing() {
        let mut vta = NiiVolume::zeros([4, 3, 3], diagonal_affine([1.0; 3], [0.0; 3]));
        for i in 1..3 {
            let n = vta.index(i, 1, 1);
            vta.data[n] = 1.0;
        }
        let set = FibreSet::from_fibres(&[fibre(1.0, 0.0, 3.0)]).unwrap();
        let cancel = AtomicBool::new(false);
        let v = fibre_values(&set, &vta, BINARY_THRESHOLD, &cancel).unwrap();
        assert_eq!(v.sum, vec![2.0]);
        assert_eq!(v.peak5, vec![1.0]);
        let none = fibre_values(&set, &vta, 5.0, &cancel).unwrap();
        assert!(none.fiber_index.is_empty());
        assert_eq!(none.total_fibers, 1);
    }

    #[test]
    fn cancel_is_an_error() {
        let set = FibreSet::from_fibres(&[fibre(1.0, 0.0, 4.0)]).unwrap();
        let cancel = AtomicBool::new(true);
        assert_eq!(
            fibre_values(&set, &field(), 200.0, &cancel),
            Err(MapError::Cancelled)
        );
    }

    #[test]
    fn feature_matrix_keeps_only_connected_rows() {
        let set = FibreSet::from_fibres(&[
            fibre(1.0, 0.0, 4.0),
            fibre(2.0, 0.0, 4.0),
            fibre(1.0, 0.0, 3.0),
        ])
        .unwrap();
        let cancel = AtomicBool::new(false);
        let a = fibre_values(&set, &field(), 200.0, &cancel).unwrap();
        let mut other = field();
        let n = other.index(2, 2, 1);
        other.data[n] = 900.0;
        let b = fibre_values(&set, &other, 200.0, &cancel).unwrap();
        let m = FeatureMatrix::from_columns(&[a, b]).unwrap();
        assert_eq!(m.connected, vec![0, 1, 2]);
        assert_eq!(m.value("peak", 1, 0), Some(0.0));
        assert_eq!(m.value("peak", 1, 1), Some(900.0));
        assert_eq!(m.value("sum", 0, 0), Some(800.0));
        assert_eq!(m.value("nope", 0, 0), None);
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<FeatureMatrix>(&json).unwrap(), m);
    }

    #[test]
    fn fibervals_and_subset_files_read_back() {
        let set = FibreSet::from_fibres(&[fibre(1.0, 0.0, 4.0), fibre(2.0, 0.0, 1.0)]).unwrap();
        let cancel = AtomicBool::new(false);
        let v = fibre_values(&set, &field(), 200.0, &cancel).unwrap();
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("v.mat");
        let meta = FibervalsMeta {
            connectome_tag: "T".into(),
            ..FibervalsMeta::default()
        };
        write_fibervals_mat(&p, &v, &meta).unwrap();
        let vars = read_mat5_file(&p).unwrap();
        let get = |n: &str| vars.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
        assert_eq!(get("total_fibers"), Some(Mat5Value::scalar(2.0)));
        assert_eq!(get("threshold_vpm"), Some(Mat5Value::scalar(200.0)));
        assert_eq!(get("connectome_tag"), Some(Mat5Value::char("T")));
        let s = d.path().join("s.mat");
        write_subset_mat(&s, &set, &[1, 0], &[2.0, 1.0], "T").unwrap();
        let vars = read_mat5_file(&s).unwrap();
        let fib = vars.iter().find(|(k, _)| k == "fibers").unwrap();
        assert_eq!(
            fib.1.dims(),
            vec![set.fibre(1).len() + set.fibre(0).len(), 4]
        );
        let src = vars
            .iter()
            .find(|(k, _)| k == "source_fiber_index")
            .unwrap();
        assert_eq!(
            src.1,
            Mat5Value::Double {
                dims: vec![2, 1],
                data: vec![2.0, 1.0]
            }
        );
        assert!(vars
            .iter()
            .any(|(k, v)| k == "ea_fibformat" && *v == Mat5Value::char("1.1")));
    }
}
