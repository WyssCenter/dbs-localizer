// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use fastwarp_matfile::{mat5::write_mat5_file, Mat5Value};

use crate::error::{format_err, Error, Result};
use crate::matio::{self, Visit};
use crate::Affine;

/// Where the points of a [`FibreSet`] came from; the points themselves are always millimetres.
#[derive(Clone, Debug, PartialEq)]
pub enum FibreSpace {
    /// Stored in millimetres.
    Mm,
    /// Stored as 1-based voxel coordinates and converted with this SPM matrix on reading.
    ConvertedFromVoxels(Affine),
}

/// All fibres of a connectome: points stacked fibre after fibre, in millimetres.
#[derive(Clone, Debug, PartialEq)]
pub struct FibreSet {
    pub points: Vec<[f32; 3]>,
    /// `K + 1` prefix sums into `points`; fibre `f` is `offsets[f]..offsets[f + 1]`.
    pub fibre_offsets: Vec<u32>,
    pub space: FibreSpace,
    /// Per-fibre value (`vals`); empty when the file has none.
    pub vals: Vec<f32>,
    /// The `mirrored` flag of a multi-tract pathway file, when present.
    pub mirrored: Option<bool>,
}

impl FibreSet {
    /// A set in millimetres from per-fibre point lists.
    pub fn from_fibres(fibres: &[Vec<[f32; 3]>]) -> Result<Self> {
        let mut points = Vec::new();
        let mut fibre_offsets = vec![0u32];
        for f in fibres {
            points.extend_from_slice(f);
            let end = u32::try_from(points.len())
                .map_err(|_| format_err("more than 2^32 - 1 points in one connectome"))?;
            fibre_offsets.push(end);
        }
        Ok(Self {
            points,
            fibre_offsets,
            space: FibreSpace::Mm,
            vals: Vec::new(),
            mirrored: None,
        })
    }

    pub fn n_fibres(&self) -> usize {
        self.fibre_offsets.len().saturating_sub(1)
    }

    /// The points of fibre `f` (empty when out of range).
    pub fn fibre(&self, f: usize) -> &[[f32; 3]] {
        match (self.fibre_offsets.get(f), self.fibre_offsets.get(f + 1)) {
            (Some(a), Some(b)) => &self.points[*a as usize..*b as usize],
            _ => &[],
        }
    }

    /// The number of points of each fibre (the `idx` variable).
    pub fn lengths(&self) -> impl Iterator<Item = u32> + '_ {
        self.fibre_offsets.windows(2).map(|w| w[1] - w[0])
    }
}

/// MATLAB dimensions of an HDF5 dataset of dataspace `h5_dims` (`specs/15` §3.2.1).
pub fn matlab_dims_from_hdf5(h5_dims: &[usize]) -> Vec<usize> {
    h5_dims.iter().rev().copied().collect()
}

pub(crate) fn fibers_layout(dims: &[usize]) -> Result<(usize, usize)> {
    match dims {
        [p, c] if *c == 4 || *c == 3 => Ok((*p, *c)),
        _ => Err(format_err(format!(
            "`fibers` must be P x 4 (or P x 3 with `idx`), found {}",
            dims.iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(" x ")
        ))),
    }
}

const STREAM_CHUNK: usize = 1 << 20;

pub(crate) struct SmallVars {
    pub fibformat: Option<String>,
    pub voxmm: Option<String>,
    pub idx: Option<Vec<f64>>,
    pub mat: Option<Vec<f64>>,
    pub vals: Option<Vec<f64>>,
    pub mirrored: Option<f64>,
}

#[cfg(test)]
pub(crate) struct RawFibres {
    pub fibers_dims: Vec<usize>,
    pub fibers: Vec<f64>,
    pub idx: Option<Vec<f64>>,
    pub fibformat: Option<String>,
    pub voxmm: Option<String>,
    pub mat: Option<Vec<f64>>,
    pub vals: Option<Vec<f64>>,
    pub mirrored: Option<f64>,
}

fn as_count(v: f64, what: &str) -> Result<u32> {
    if v.is_finite() && v.fract() == 0.0 && v > 0.0 && v <= f64::from(u32::MAX) {
        Ok(v as u32)
    } else {
        Err(format_err(format!("{what} {v} is not a positive integer")))
    }
}

struct Collector<'a> {
    points: Vec<[f32; 3]>,
    offsets: &'a [u32],
    all_non_negative: bool,
    cancel: Option<&'a AtomicBool>,
}

impl Collector<'_> {
    fn visit(&mut self, col: usize, chunk: &[f32], first: usize) -> Result<()> {
        if self.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Error::Cancelled);
        }
        let end = first
            .checked_add(chunk.len())
            .filter(|e| *e <= self.points.len())
            .ok_or_else(|| format_err("`fibers` column is longer than its dimensions"))?;
        if col < 3 {
            if chunk.iter().any(|v| !v.is_finite()) {
                return Err(format_err("non-finite coordinate"));
            }
            if chunk.iter().any(|v| *v < 0.0) {
                self.all_non_negative = false;
            }
            for (p, v) in self.points[first..end].iter_mut().zip(chunk) {
                p[col] = *v;
            }
            return Ok(());
        }
        let k = self.offsets.len() - 1;
        let mut f = self
            .offsets
            .partition_point(|o| (*o as usize) <= first)
            .saturating_sub(1)
            .min(k.saturating_sub(1));
        for (i, v) in chunk.iter().enumerate() {
            let at = first + i;
            while self.offsets[f + 1] as usize <= at {
                f += 1;
            }
            if *v != (f + 1) as f32 {
                return Err(format_err(format!(
                    "fibre number column is not constant {} over fibre {}",
                    f + 1,
                    f + 1
                )));
            }
        }
        Ok(())
    }
}

pub(crate) fn assemble(
    small: SmallVars,
    dims: &[usize],
    cancel: Option<&AtomicBool>,
    feed: impl FnOnce(&[usize], &mut Visit<'_>) -> Result<()>,
) -> Result<FibreSet> {
    match small.fibformat.as_deref() {
        Some("1.0" | "1.1") => {}
        Some(other) => {
            return Err(format_err(format!(
                "unknown `ea_fibformat` {other:?}; expected 1.0 or 1.1"
            )))
        }
        None => {
            return Err(format_err(
                "legacy fibre format (no `ea_fibformat`); convert it with Lead-DBS or re-export",
            ))
        }
    }
    let (p, cols) = fibers_layout(dims)?;
    let idx = small
        .idx
        .ok_or_else(|| format_err("variable `idx` is missing"))?;
    let mut offsets = Vec::with_capacity(idx.len() + 1);
    offsets.push(0u32);
    let mut total = 0u64;
    for v in &idx {
        let n = as_count(*v, "`idx` entry")?;
        total += u64::from(n);
        offsets.push(
            u32::try_from(total)
                .map_err(|_| format_err("more than 2^32 - 1 points in one connectome"))?,
        );
    }
    drop(idx);
    if total != p as u64 {
        return Err(format_err(format!(
            "sum of `idx` ({total}) differs from the number of points ({p})"
        )));
    }
    let mut collector = Collector {
        points: vec![[0.0; 3]; p],
        offsets: &offsets,
        all_non_negative: true,
        cancel,
    };
    let wanted: Vec<usize> = (0..cols).collect();
    feed(&wanted, &mut |c, chunk, first| {
        collector.visit(c, chunk, first)
    })?;
    let Collector {
        mut points,
        all_non_negative,
        ..
    } = collector;
    let voxels = match small.voxmm.as_deref() {
        Some("mm") => false,
        Some("vox") => true,
        Some(other) => return Err(format_err(format!("`voxmm` is {other:?}, not mm or vox"))),
        None => all_non_negative,
    };
    let space = if voxels {
        let m = small.mat.ok_or_else(|| {
            format_err("`voxmm` is vox but `mat` is missing and no reference image was given")
        })?;
        if m.len() != 16 {
            return Err(format_err("`mat` is not 4 x 4"));
        }
        let mut affine = [[0.0; 4]; 4];
        for (r, row) in affine.iter_mut().enumerate() {
            for (c, v) in row.iter_mut().enumerate() {
                *v = m[c * 4 + r];
            }
        }
        for q in &mut points {
            *q = apply(&affine, [f64::from(q[0]), f64::from(q[1]), f64::from(q[2])]);
        }
        FibreSpace::ConvertedFromVoxels(affine)
    } else {
        FibreSpace::Mm
    };
    let k = offsets.len() - 1;
    let vals = match small.vals {
        Some(v) if v.len() == k => v.into_iter().map(|x| x as f32).collect(),
        Some(v) if v.is_empty() => Vec::new(),
        Some(v) => {
            return Err(format_err(format!(
                "`vals` has {} entries for {k} fibres",
                v.len()
            )))
        }
        None => Vec::new(),
    };
    Ok(FibreSet {
        points,
        fibre_offsets: offsets,
        space,
        vals,
        mirrored: small.mirrored.map(|v| v != 0.0),
    })
}

#[cfg(test)]
pub(crate) fn fibres_from_raw(raw: RawFibres) -> Result<FibreSet> {
    let small = SmallVars {
        fibformat: raw.fibformat,
        voxmm: raw.voxmm,
        idx: raw.idx,
        mat: raw.mat,
        vals: raw.vals,
        mirrored: raw.mirrored,
    };
    let fibers = raw.fibers;
    let dims = raw.fibers_dims;
    let layout = fibers_layout(&dims);
    assemble(small, &dims, None, |wanted, visit| {
        let (p, cols) = layout?;
        if fibers.len() != p * cols {
            return Err(format_err(
                "`fibers` data length does not match its dimensions",
            ));
        }
        let mut buf = Vec::new();
        for &c in wanted {
            for start in (0..p).step_by(STREAM_CHUNK) {
                let end = (start + STREAM_CHUNK).min(p);
                buf.clear();
                buf.extend(fibers[c * p + start..c * p + end].iter().map(|v| *v as f32));
                visit(c, &buf, start)?;
            }
        }
        Ok(())
    })
}

fn apply(m: &Affine, v: [f64; 3]) -> [f32; 3] {
    let row = |r: usize| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2] + m[r][3];
    [row(0) as f32, row(1) as f32, row(2) as f32]
}

/// Reads a structural `data.mat` (MAT v5 / v7 or v7.3) into a [`FibreSet`] in millimetres (`specs/15` §3.2).
///
/// `fibers` is streamed in chunks of 2^20 values straight into the point array (columns 0 to 2)
/// and checked against `idx` (column 3); no copy of the whole variable is made.
pub fn read_data_mat(path: &Path) -> Result<FibreSet> {
    read_data_mat_cancellable(path, None)
}

/// [`read_data_mat`] that stops with [`Error::Cancelled`] soon after `cancel` is set.
pub fn read_data_mat_cancellable(path: &Path, cancel: Option<&AtomicBool>) -> Result<FibreSet> {
    let src = matio::open_streaming(path)?;
    let dims = src
        .shape("fibers")?
        .ok_or_else(|| format_err(format!("{}: variable `fibers` is missing", path.display())))?;
    let small = SmallVars {
        fibformat: src.text("ea_fibformat")?,
        voxmm: src.text("voxmm")?,
        idx: src.numeric("idx")?.map(|n| n.data),
        mat: src.numeric("mat")?.map(|n| n.data),
        vals: src.numeric("vals")?.map(|n| n.data),
        mirrored: src
            .numeric("mirrored")?
            .and_then(|n| n.data.first().copied()),
    };
    assemble(small, &dims, cancel, |wanted, visit| {
        src.stream_cols("fibers", wanted, STREAM_CHUNK, visit)
    })
    .map_err(|e| match e {
        Error::Format(m) => Error::Format(format!("{}: {m}", path.display())),
        other => other,
    })
}

/// Reads `data.mat`, `data.trk` or `data.trk.gz` by file name.
pub fn read_fibres(path: &Path) -> Result<FibreSet> {
    read_fibres_cancellable(path, None)
}

/// [`read_fibres`] that stops with [`Error::Cancelled`] soon after `cancel` is set (`.trk` files are read in one piece).
pub fn read_fibres_cancellable(path: &Path, cancel: Option<&AtomicBool>) -> Result<FibreSet> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.ends_with(".trk") || name.ends_with(".trk.gz") {
        crate::trk::read_trk(path, None)
    } else {
        read_data_mat_cancellable(path, cancel)
    }
}

/// The number of points in a fibre file, from its header only (an estimate for `.trk` files: 12 bytes per point, a third as many for `.trk.gz`).
pub fn probe_point_count(path: &Path) -> Result<u64> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.ends_with(".trk") || name.ends_with(".trk.gz") {
        let size = std::fs::metadata(path)
            .map_err(|e| format_err(format!("{}: {e}", path.display())))?
            .len();
        return Ok(if name.ends_with(".gz") {
            size / 4
        } else {
            size / 12
        });
    }
    let src = matio::open_streaming(path)?;
    let dims = src
        .shape("fibers")?
        .ok_or_else(|| format_err(format!("{}: variable `fibers` is missing", path.display())))?;
    Ok(fibers_layout(&dims)?.0 as u64)
}

/// Writes `set` as a MAT v7 file in the `ea_fibformat` 1.0 layout (millimetres).
pub fn write_data_mat(path: &Path, set: &FibreSet) -> Result<()> {
    let p = set.points.len();
    let k = set.n_fibres();
    let mut fibers = Vec::with_capacity(p * 4);
    for c in 0..3 {
        fibers.extend(set.points.iter().map(|q| f64::from(q[c])));
    }
    for (f, w) in set.fibre_offsets.windows(2).enumerate() {
        fibers.extend(std::iter::repeat_n((f + 1) as f64, (w[1] - w[0]) as usize));
    }
    let col = |data: Vec<f64>| Mat5Value::Double {
        dims: vec![data.len(), 1],
        data,
    };
    let mut vars = vec![
        (
            "fibers".to_owned(),
            Mat5Value::Double {
                dims: vec![p, 4],
                data: fibers,
            },
        ),
        (
            "idx".to_owned(),
            col(set.lengths().map(f64::from).collect()),
        ),
        ("ea_fibformat".to_owned(), Mat5Value::char("1.0")),
        ("fourindex".to_owned(), Mat5Value::scalar(1.0)),
        ("voxmm".to_owned(), Mat5Value::char("mm")),
    ];
    if set.vals.len() == k && k > 0 {
        vars.push((
            "vals".to_owned(),
            col(set.vals.iter().map(|v| f64::from(*v)).collect()),
        ));
    }
    if let Some(m) = set.mirrored {
        vars.push((
            "mirrored".to_owned(),
            Mat5Value::scalar(f64::from(u8::from(m))),
        ));
    }
    write_mat5_file(path, &vars, true).map_err(|e| Error::Mat {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn sample() -> FibreSet {
        let mut s = FibreSet::from_fibres(&[
            vec![[-1.0, 0.0, 0.5], [-1.0, 1.0, 0.5]],
            vec![[3.0, 3.0, 3.0], [4.0, 3.0, 3.0], [5.0, 3.0, 3.0]],
            vec![[-8.0, -8.0, -8.0], [-8.0, -9.0, -8.0]],
        ])
        .unwrap();
        s.vals = vec![1.0, 2.0, 3.0];
        s
    }

    fn raw_from(set: &FibreSet) -> RawFibres {
        let p = set.points.len();
        let mut fibers = Vec::new();
        for c in 0..3 {
            fibers.extend(set.points.iter().map(|q| f64::from(q[c])));
        }
        for (f, w) in set.fibre_offsets.windows(2).enumerate() {
            fibers.extend(std::iter::repeat_n((f + 1) as f64, (w[1] - w[0]) as usize));
        }
        RawFibres {
            fibers_dims: vec![p, 4],
            fibers,
            idx: Some(set.lengths().map(f64::from).collect()),
            fibformat: Some("1.0".into()),
            voxmm: Some("mm".into()),
            mat: None,
            vals: None,
            mirrored: None,
        }
    }

    #[test]
    fn round_trip_through_a_v7_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.mat");
        let set = sample();
        write_data_mat(&path, &set).unwrap();
        let back = read_data_mat(&path).unwrap();
        assert_eq!(back, set);
        assert_eq!(back.n_fibres(), 3);
        assert_eq!(back.fibre(1).len(), 3);
        assert_eq!(read_fibres(&path).unwrap(), set);
    }

    #[test]
    fn mismatched_idx_total_is_an_error() {
        let mut raw = raw_from(&sample());
        raw.idx = Some(vec![2.0, 3.0, 3.0]);
        let e = fibres_from_raw(raw).unwrap_err();
        assert!(e.to_string().contains("E_CONN_FORMAT"), "{e}");
        assert!(e.to_string().contains("sum of `idx`"), "{e}");
    }

    #[test]
    fn idx_must_be_positive_integers() {
        for bad in [[2.0, 0.0, 5.0], [2.0, -1.0, 6.0], [2.5, 2.5, 2.0]] {
            let mut raw = raw_from(&sample());
            raw.idx = Some(bad.to_vec());
            assert!(fibres_from_raw(raw).is_err());
        }
    }

    #[test]
    fn fibre_number_column_is_checked() {
        let mut raw = raw_from(&sample());
        let p = raw.fibers_dims[0];
        raw.fibers[3 * p + 2] = 9.0;
        assert!(fibres_from_raw(raw).is_err());
    }

    #[test]
    fn legacy_and_bad_shapes_are_errors() {
        let mut raw = raw_from(&sample());
        raw.fibformat = None;
        assert!(fibres_from_raw(raw)
            .unwrap_err()
            .to_string()
            .contains("legacy"));
        let mut raw = raw_from(&sample());
        raw.fibers_dims = vec![4, 7];
        assert!(fibres_from_raw(raw).is_err());
    }

    #[test]
    fn three_columns_with_idx_are_accepted() {
        let set = sample();
        let mut raw = raw_from(&set);
        let p = raw.fibers_dims[0];
        raw.fibers.truncate(3 * p);
        raw.fibers_dims = vec![p, 3];
        assert_eq!(fibres_from_raw(raw).unwrap().points, set.points);
    }

    #[test]
    fn voxel_coordinates_use_the_one_based_matrix() {
        let mut raw = raw_from(&sample());
        raw.voxmm = Some("vox".into());
        assert!(fibres_from_raw(RawFibres {
            fibers: raw.fibers.clone(),
            idx: raw.idx.clone(),
            fibformat: raw.fibformat.clone(),
            voxmm: raw.voxmm.clone(),
            fibers_dims: raw.fibers_dims.clone(),
            mat: None,
            vals: None,
            mirrored: None,
        })
        .is_err());
        let mut m = vec![0.0; 16];
        m[0] = 2.0;
        m[5] = 2.0;
        m[10] = 2.0;
        m[15] = 1.0;
        m[12] = -10.0;
        m[13] = -20.0;
        m[14] = -30.0;
        raw.mat = Some(m);
        let set = fibres_from_raw(raw).unwrap();
        assert_eq!(set.points[0], [-12.0, -20.0, -29.0]);
        assert!(matches!(set.space, FibreSpace::ConvertedFromVoxels(_)));
    }

    #[test]
    fn missing_voxmm_is_inferred_from_the_sign() {
        let mut raw = raw_from(&sample());
        raw.voxmm = None;
        assert_eq!(fibres_from_raw(raw).unwrap().space, FibreSpace::Mm);
        let positive = FibreSet::from_fibres(&[vec![[1.0, 1.0, 1.0]]]).unwrap();
        let mut raw = raw_from(&positive);
        raw.voxmm = None;
        assert!(fibres_from_raw(raw).is_err());
    }

    #[test]
    fn hdf5_dataspace_is_transposed_to_matlab_dims() {
        let p = 396_595_508usize;
        let dims = matlab_dims_from_hdf5(&[4, p]);
        assert_eq!(dims, vec![p, 4]);
        assert_eq!(fibers_layout(&dims).unwrap(), (p, 4));
        assert!(fibers_layout(&[4, p]).is_err());
        assert_eq!(matlab_dims_from_hdf5(&[1, 8]), vec![8, 1]);
    }

    #[test]
    fn column_major_storage_gives_x_then_y_then_z() {
        let set = sample();
        let raw = raw_from(&set);
        let back = fibres_from_raw(raw).unwrap();
        assert_eq!(back.points, set.points);
    }
}
