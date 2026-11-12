// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! Real-subject A/B harness: our SyN outputs against the ANTs outputs of the same stage.
//!
//! A subject folder holds `moving_t1_N4.nii.gz`, optionally `moving_t2_in_t1.nii.gz`,
//! `ants_Composite.h5`, `ants_InverseComposite.h5` and `reconstruction.json`; the templates
//! `template_t1.nii` (and `template_t2.nii`) sit in the folder or its parent.

use std::path::{Path, PathBuf};

use fastwarp_reg::grid::Grid;
use fastwarp_reg::linalg::{mat3_det, mat3_inverse, mat3_mul, Mat3};
use fastwarp_reg::recipe::SynRecipe;
use fastwarp_reg::register::resample_isotropic;
use fastwarp_reg::transform::{
    read_transform_file, AffineTransform, TransformChain, TransformElem,
};
use fastwarp_reg::{DisplacementField, Volume};
use rayon::prelude::*;
use serde::Serialize;

/// Boxed error of the harness.
pub type AbError = Box<dyn std::error::Error + Send + Sync>;
/// Result alias of the harness.
pub type AbResult<T> = Result<T, AbError>;

/// Fraction of the 99th intensity percentile of the template above which a voxel counts as brain.
pub const BRAIN_MASK_FRACTION: f64 = 0.1;

fn err(msg: impl Into<String>) -> AbError {
    msg.into().into()
}

/// Input files of one subject.
#[derive(Debug, Clone)]
pub struct SubjectFiles {
    /// Template T1w.
    pub template_t1: PathBuf,
    /// Template T2w, when the subject has a T2w channel.
    pub template_t2: Option<PathBuf>,
    /// Moving T1w.
    pub moving_t1: PathBuf,
    /// Moving T2w in T1w space.
    pub moving_t2: Option<PathBuf>,
    /// ANTs `Composite.h5`.
    pub ants_composite: PathBuf,
    /// ANTs `InverseComposite.h5`.
    pub ants_inverse: PathBuf,
    /// Reco v2 JSON.
    pub reconstruction: PathBuf,
}

impl SubjectFiles {
    /// Resolves the files of a subject folder; templates may be in the folder or its parent.
    pub fn from_dir(dir: &Path) -> AbResult<Self> {
        let find_template = |name: &str| -> Option<PathBuf> {
            let here = dir.join(name);
            if here.is_file() {
                return Some(here);
            }
            let up = dir.parent()?.join(name);
            up.is_file().then_some(up)
        };
        let need = |name: &str| -> AbResult<PathBuf> {
            let p = dir.join(name);
            if p.is_file() {
                Ok(p)
            } else {
                Err(err(format!("missing {}", p.display())))
            }
        };
        let template_t1 =
            find_template("template_t1.nii").ok_or_else(|| err("missing template_t1.nii"))?;
        let moving_t2 = dir.join("moving_t2_in_t1.nii.gz");
        let template_t2 = find_template("template_t2.nii");
        let multispectral = moving_t2.is_file() && template_t2.is_some();
        Ok(Self {
            template_t1,
            template_t2: if multispectral { template_t2 } else { None },
            moving_t1: need("moving_t1_N4.nii.gz")?,
            moving_t2: multispectral.then_some(moving_t2),
            ants_composite: need("ants_Composite.h5")?,
            ants_inverse: need("ants_InverseComposite.h5")?,
            reconstruction: need("reconstruction.json")?,
        })
    }

    /// The recipe matching the channels present.
    pub fn recipe(&self) -> SynRecipe {
        if self.moving_t2.is_some() {
            SynRecipe::cohort_multispectral()
        } else {
            SynRecipe::cohort()
        }
    }
}

/// Loads a NIfTI as an `f64` volume.
pub fn load_f64(path: &Path) -> AbResult<Volume<f64>> {
    Ok(Volume::<f32>::load_nifti(path)?.to_f64())
}

/// Element 0 of an ANTs `Composite.h5` when it is an affine.
pub fn initial_affine_from_composite(path: &Path) -> AbResult<AffineTransform> {
    let chain = read_transform_file(path)?;
    match chain.queue().first() {
        Some(TransformElem::Affine(a)) => Ok(*a),
        _ => Err(err(format!(
            "{}: element 0 is not an affine transform",
            path.display()
        ))),
    }
}

/// Fields of a forward composite `[Affine, Warp]` and of an inverse composite
/// `[InverseWarp, Affine^-1]`.
pub fn fields_of_pair(
    forward: &TransformChain,
    inverse: &TransformChain,
) -> AbResult<(DisplacementField, DisplacementField)> {
    let fw = match forward.queue().get(1) {
        Some(TransformElem::DisplacementField(f)) => f.clone(),
        _ => return Err(err("forward composite has no warp as element 1")),
    };
    let iw = match inverse.queue().first() {
        Some(TransformElem::DisplacementField(f)) => f.clone(),
        _ => return Err(err("inverse composite has no warp as element 0")),
    };
    Ok((fw, iw))
}

/// Boolean brain mask on `grid`: template intensity above `fraction` of its 99th percentile.
pub fn brain_mask(template_on_grid: &Volume<f64>, fraction: f64) -> Vec<bool> {
    let mut v: Vec<f64> = template_on_grid.data().to_vec();
    if v.is_empty() {
        return Vec::new();
    }
    let k = ((v.len() as f64) * 0.99) as usize;
    let k = k.min(v.len() - 1);
    let (_, p99, _) = v.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    let threshold = fraction * *p99;
    template_on_grid
        .data()
        .iter()
        .map(|x| *x > threshold)
        .collect()
}

/// Statistics of `|a - b|` between two displacement fields, in millimetres.
#[derive(Debug, Clone, Serialize, Default)]
pub struct FieldDiff {
    /// Voxels compared.
    pub voxels: usize,
    /// Root mean square of the difference norm.
    pub rms_mm: f64,
    /// Mean difference norm.
    pub mean_mm: f64,
    /// 95th percentile of the difference norm.
    pub p95_mm: f64,
    /// Largest difference norm.
    pub max_mm: f64,
    /// RMS norm of field `a` (scale reference).
    pub a_rms_mm: f64,
    /// RMS norm of field `b`.
    pub b_rms_mm: f64,
}

/// Compares two fields at the voxels of `a`'s grid selected by `mask`; `b` is sampled linearly
/// at those points (identical grids sample voxel centres). Voxels where `b` has no value are
/// skipped.
pub fn compare_fields(
    a: &DisplacementField,
    b: &DisplacementField,
    mask: &[bool],
) -> AbResult<FieldDiff> {
    let g = a.grid();
    if mask.len() != g.num_voxels() {
        return Err(err("mask does not match the grid of the first field"));
    }
    let same = g.approx_eq(b.grid(), 1e-6);
    let rows: Vec<(f64, f64, f64)> = (0..g.num_voxels())
        .into_par_iter()
        .filter(|&lin| mask[lin])
        .filter_map(|lin| {
            let [i, j, k] = g.ijk(lin);
            let ua = a.get(i, j, k)?;
            let ub = if same {
                b.get(i, j, k)?
            } else {
                b.displacement_at(g.index_to_point([i as f64, j as f64, k as f64]))?
            };
            let d = [ua[0] - ub[0], ua[1] - ub[1], ua[2] - ub[2]];
            Some((
                (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt(),
                ua[0] * ua[0] + ua[1] * ua[1] + ua[2] * ua[2],
                ub[0] * ub[0] + ub[1] * ub[1] + ub[2] * ub[2],
            ))
        })
        .collect();
    if rows.is_empty() {
        return Err(err("no voxel to compare"));
    }
    let n = rows.len() as f64;
    let mut norms: Vec<f64> = rows.iter().map(|r| r.0).collect();
    let sum_sq: f64 = norms.iter().map(|x| x * x).sum();
    let sum: f64 = norms.iter().sum();
    let a_sq: f64 = rows.iter().map(|r| r.1).sum();
    let b_sq: f64 = rows.iter().map(|r| r.2).sum();
    let max = norms.iter().cloned().fold(0.0, f64::max);
    let k = (((norms.len() as f64) * 0.95) as usize).min(norms.len() - 1);
    let (_, p95, _) = norms.select_nth_unstable_by(k, |x, y| x.total_cmp(y));
    Ok(FieldDiff {
        voxels: rows.len(),
        rms_mm: (sum_sq / n).sqrt(),
        mean_mm: sum / n,
        p95_mm: *p95,
        max_mm: max,
        a_rms_mm: (a_sq / n).sqrt(),
        b_rms_mm: (b_sq / n).sqrt(),
    })
}

/// Jacobian determinant statistics of `x -> x + u(x)`.
#[derive(Debug, Clone, Serialize, Default)]
pub struct JacobianStats {
    /// Voxels evaluated.
    pub voxels: usize,
    /// Smallest determinant.
    pub min: f64,
    /// Largest determinant.
    pub max: f64,
    /// Mean determinant.
    pub mean: f64,
    /// Percentage of voxels with determinant at or below zero.
    pub percent_non_positive: f64,
}

fn index_derivative(f: &DisplacementField, i: usize, j: usize, k: usize, axis: usize) -> [f64; 3] {
    let size = f.grid().size();
    let idx = [i, j, k];
    let n = size[axis];
    if n < 2 {
        return [0.0; 3];
    }
    let lo_i = idx[axis].saturating_sub(1);
    let hi_i = (idx[axis] + 1).min(n - 1);
    let mut lo = idx;
    let mut hi = idx;
    lo[axis] = lo_i;
    hi[axis] = hi_i;
    let a = f.get(lo[0], lo[1], lo[2]).unwrap_or([0.0; 3]);
    let b = f.get(hi[0], hi[1], hi[2]).unwrap_or([0.0; 3]);
    let h = (hi_i - lo_i) as f64;
    [(b[0] - a[0]) / h, (b[1] - a[1]) / h, (b[2] - a[2]) / h]
}

/// Jacobian determinant of `x + u(x)` in physical coordinates (central differences, one-sided
/// on the faces), over the voxels selected by `mask` (all when `None`).
pub fn jacobian_stats(f: &DisplacementField, mask: Option<&[bool]>) -> AbResult<JacobianStats> {
    let g: &Grid = f.grid();
    let s = g.spacing();
    let d = g.direction();
    let offset: Mat3 = mat3_mul(&d, &[[s[0], 0.0, 0.0], [0.0, s[1], 0.0], [0.0, 0.0, s[2]]]);
    let inv = mat3_inverse(&offset).ok_or_else(|| err("singular grid"))?;
    let rows: Vec<f64> = (0..g.num_voxels())
        .into_par_iter()
        .filter(|&lin| mask.map(|m| m[lin]).unwrap_or(true))
        .map(|lin| {
            let [i, j, k] = g.ijk(lin);
            let du: [[f64; 3]; 3] = [
                index_derivative(f, i, j, k, 0),
                index_derivative(f, i, j, k, 1),
                index_derivative(f, i, j, k, 2),
            ];
            let mut dmat = [[0.0; 3]; 3];
            for c in 0..3 {
                for a in 0..3 {
                    dmat[c][a] = du[a][c];
                }
            }
            let phys = mat3_mul(&dmat, &inv);
            let mut jm = phys;
            for (q, row) in jm.iter_mut().enumerate() {
                row[q] += 1.0;
            }
            mat3_det(&jm)
        })
        .collect();
    if rows.is_empty() {
        return Err(err("no voxel for the Jacobian"));
    }
    let n = rows.len() as f64;
    Ok(JacobianStats {
        voxels: rows.len(),
        min: rows.iter().cloned().fold(f64::INFINITY, f64::min),
        max: rows.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        mean: rows.iter().sum::<f64>() / n,
        percent_non_positive: 100.0 * rows.iter().filter(|v| **v <= 0.0).count() as f64 / n,
    })
}

/// One side of a Reco v2 file: key, native RAS points, template RAS points (contacts, then head
/// and tail).
pub type SidePoints = (String, Vec<[f64; 3]>, Vec<[f64; 3]>);

/// Contact positions of a Reco v2 file.
#[derive(Debug, Clone, Default)]
pub struct RecoPoints {
    /// `(side key, native RAS points, template RAS points)`; contacts, then head and tail.
    pub sides: Vec<SidePoints>,
}

fn triplets(v: &serde_json::Value) -> Vec<[f64; 3]> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let p = p.as_array()?;
                    Some([
                        p.first()?.as_f64()?,
                        p.get(1)?.as_f64()?,
                        p.get(2)?.as_f64()?,
                    ])
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Reads `native` and `mni` contacts, head and tail of every side.
pub fn read_reco_points(path: &Path) -> AbResult<RecoPoints> {
    let text = std::fs::read_to_string(path)?;
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let mut out = RecoPoints::default();
    let native = v.get("native").ok_or_else(|| err("no native block"))?;
    let mni = v.get("mni").ok_or_else(|| err("no mni block"))?;
    let keys: Vec<String> = native
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    for key in keys {
        let (Some(n), Some(m)) = (native.get(&key), mni.get(&key)) else {
            continue;
        };
        let collect = |side: &serde_json::Value| -> Vec<[f64; 3]> {
            let mut pts = triplets(&side["coords_mm"]);
            for name in ["head", "tail"] {
                let mut t = triplets(&serde_json::Value::Array(vec![
                    side["markers"][name].clone()
                ]));
                pts.append(&mut t);
            }
            pts
        };
        out.sides.push((key, collect(n), collect(m)));
    }
    out.sides.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Per-side distance statistics between two point sets.
#[derive(Debug, Clone, Serialize, Default)]
pub struct PointDiff {
    /// Side key of the Reco file (`1` right, `2` left).
    pub side: String,
    /// Points compared.
    pub points: usize,
    /// Mean distance in millimetres.
    pub mean_mm: f64,
    /// Largest distance.
    pub max_mm: f64,
    /// Distances in input order.
    pub distances_mm: Vec<f64>,
}

/// Distances between `a[i]` and `b[i]`, per side.
pub fn point_diff(side: &str, a: &[[f64; 3]], b: &[[f64; 3]]) -> PointDiff {
    let distances_mm: Vec<f64> = a
        .iter()
        .zip(b)
        .map(|(p, q)| {
            ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
        })
        .collect();
    let n = distances_mm.len().max(1) as f64;
    PointDiff {
        side: side.to_string(),
        points: distances_mm.len(),
        mean_mm: distances_mm.iter().sum::<f64>() / n,
        max_mm: distances_mm.iter().cloned().fold(0.0, f64::max),
        distances_mm,
    }
}

/// Difference of two linear transforms (fixed to moving points).
#[derive(Debug, Clone, Serialize, Default)]
pub struct AffineDiff {
    /// Voxels of the mask the displacement was evaluated at.
    pub voxels: usize,
    /// Largest distance between the two images of a mask voxel centre.
    pub max_mm: f64,
    /// Mean of the same.
    pub mean_mm: f64,
    /// Root mean square of the same.
    pub rms_mm: f64,
    /// Largest absolute difference of the 3x3 matrix elements.
    pub matrix_max_abs: f64,
    /// Difference of the offsets `t + c - M c` (millimetres).
    pub offset_diff: [f64; 3],
    /// Determinant of our matrix.
    pub det_ours: f64,
    /// Determinant of the ANTs matrix.
    pub det_ants: f64,
    /// The 12 parameters of ours (centre-independent: `matrix`, `offset`).
    pub ours: [f64; 12],
    /// The 12 parameters of the ANTs transform (matrix, offset).
    pub ants: [f64; 12],
}

fn matrix_offset_params(a: &AffineTransform) -> [f64; 12] {
    let mut out = [0.0; 12];
    let off = a.offset();
    for r in 0..3 {
        for c in 0..3 {
            out[r * 3 + c] = a.matrix[r][c];
        }
        out[9 + r] = off[r];
    }
    out
}

/// Compares two linear transforms at the centres of the voxels selected by `mask` on `grid`.
pub fn compare_affines(
    ours: &AffineTransform,
    ants: &AffineTransform,
    grid: &Grid,
    mask: &[bool],
) -> AffineDiff {
    let (mut max, mut sum, mut sum2, mut n) = (0.0f64, 0.0f64, 0.0f64, 0usize);
    for (lin, keep) in mask.iter().enumerate() {
        if !*keep {
            continue;
        }
        let [i, j, k] = grid.ijk(lin);
        let p = grid.index_to_point([i as f64, j as f64, k as f64]);
        let (a, b) = (ours.apply(p), ants.apply(p));
        let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
        max = max.max(d);
        sum += d;
        sum2 += d * d;
        n += 1;
    }
    let (oo, ao) = (ours.offset(), ants.offset());
    let mut matrix_max_abs = 0.0f64;
    for r in 0..3 {
        for c in 0..3 {
            matrix_max_abs = matrix_max_abs.max((ours.matrix[r][c] - ants.matrix[r][c]).abs());
        }
    }
    AffineDiff {
        voxels: n,
        max_mm: max,
        mean_mm: sum / n.max(1) as f64,
        rms_mm: (sum2 / n.max(1) as f64).sqrt(),
        matrix_max_abs,
        offset_diff: [oo[0] - ao[0], oo[1] - ao[1], oo[2] - ao[2]],
        det_ours: mat3_det(&ours.matrix),
        det_ants: mat3_det(&ants.matrix),
        ours: matrix_offset_params(ours),
        ants: matrix_offset_params(ants),
    }
}

/// Everything the A/B reports.
#[derive(Debug, Clone, Serialize, Default)]
pub struct AbReport {
    /// Linear transform (element 0 of the composite) against the ANTs one.
    pub affine_diff: AffineDiff,
    /// Mask rule.
    pub mask_rule: String,
    /// Voxels in the mask.
    pub mask_voxels: usize,
    /// Forward field difference inside the mask.
    pub forward_diff: FieldDiff,
    /// Inverse field difference inside the mask.
    pub inverse_diff: FieldDiff,
    /// Jacobian of our forward warp inside the mask.
    pub jacobian_forward_ours_mask: JacobianStats,
    /// Jacobian of the ANTs forward warp inside the mask.
    pub jacobian_forward_ants_mask: JacobianStats,
    /// Jacobian of our forward warp over the whole grid.
    pub jacobian_forward_ours_all: JacobianStats,
    /// Jacobian of the ANTs forward warp over the whole grid.
    pub jacobian_forward_ants_all: JacobianStats,
    /// Jacobian of our inverse warp inside the mask.
    pub jacobian_inverse_ours_mask: JacobianStats,
    /// Jacobian of the ANTs inverse warp inside the mask.
    pub jacobian_inverse_ants_mask: JacobianStats,
    /// Largest deviation of our reader on the ANTs inverse composite from the stored `mni`
    /// coordinates.
    pub ants_reproduction_max_mm: f64,
    /// Mean deviation of the same.
    pub ants_reproduction_mean_mm: f64,
    /// Largest deviation of the pipeline-style point warp (1 mm lattice, trilinear) on the ANTs
    /// inverse composite from the stored `mni` coordinates.
    pub ants_reproduction_pipeline_max_mm: f64,
    /// Mean deviation of the same.
    pub ants_reproduction_pipeline_mean_mm: f64,
    /// Our inverse composite (pipeline-style point warp) vs the ANTs-produced `mni` coordinates,
    /// per side.
    pub points_vs_ants: Vec<PointDiff>,
    /// Our inverse composite vs our reader on the ANTs inverse composite, per side.
    pub points_vs_ants_reader: Vec<PointDiff>,
}

/// Maps points (RAS) the way the cohort pipeline does: the chain is evaluated on the
/// integer-millimetre lattice around each point (`f32` coordinates, as the written coordinate
/// image holds them) and the corner values are interpolated trilinearly.
pub fn map_ras_pipeline_style(chain: &TransformChain, pts: &[[f64; 3]]) -> Vec<[f64; 3]> {
    fastwarp_reg::map_points_ras_lattice(chain, pts)
}

/// Our inverse composite (pipeline-style point warp) against the template-space coordinates
/// stored in a Reco v2 JSON, per side.
pub fn contacts_vs_stored(reconstruction: &Path, our_prefix: &Path) -> AbResult<Vec<PointDiff>> {
    let our_inv = read_transform_file(&fastwarp_reg::transform::ants_io::with_suffix(
        our_prefix,
        "InverseComposite.h5",
    ))?;
    let reco = read_reco_points(reconstruction)?;
    Ok(reco
        .sides
        .iter()
        .map(|(side, native, mni)| point_diff(side, &map_ras_pipeline_style(&our_inv, native), mni))
        .collect())
}

/// Maps points (RAS) through a chain.
pub fn map_ras(chain: &TransformChain, pts: &[[f64; 3]]) -> Vec<[f64; 3]> {
    fastwarp_reg::map_points_ras(chain, pts)
}

/// Runs the full comparison of our outputs under `our_prefix` against the ANTs files of
/// `files`.
pub fn compare(files: &SubjectFiles, our_prefix: &Path, iso_mm: f64) -> AbResult<AbReport> {
    let template = load_f64(&files.template_t1)?;
    let template_iso = resample_isotropic(&template, iso_mm)?;
    drop(template);
    let mask = brain_mask(&template_iso, BRAIN_MASK_FRACTION);
    let mask_voxels = mask.iter().filter(|m| **m).count();

    let ants_fw = read_transform_file(&files.ants_composite)?;
    let ants_inv = read_transform_file(&files.ants_inverse)?;
    let (ants_fwd_field, ants_inv_field) = fields_of_pair(&ants_fw, &ants_inv)?;

    let our_fw = read_transform_file(&fastwarp_reg::transform::ants_io::with_suffix(
        our_prefix,
        "Composite.h5",
    ))?;
    let our_inv_path =
        fastwarp_reg::transform::ants_io::with_suffix(our_prefix, "InverseComposite.h5");
    let our_inv = read_transform_file(&our_inv_path)?;
    let (our_fwd_field, our_inv_field) = fields_of_pair(&our_fw, &our_inv)?;

    let mut report = AbReport {
        mask_rule: format!(
            "template T1w resampled to {iso_mm} mm isotropic, voxel > {BRAIN_MASK_FRACTION} x its 99th percentile"
        ),
        mask_voxels,
        ..AbReport::default()
    };
    if let (Some(TransformElem::Affine(o)), Some(TransformElem::Affine(a))) =
        (our_fw.queue().first(), ants_fw.queue().first())
    {
        report.affine_diff = compare_affines(o, a, template_iso.grid(), &mask);
    }
    report.forward_diff = compare_fields(&our_fwd_field, &ants_fwd_field, &mask)?;
    report.inverse_diff = compare_fields(&our_inv_field, &ants_inv_field, &mask)?;
    let ours_mask_grid_ok = our_fwd_field.grid().approx_eq(template_iso.grid(), 1e-6);
    if !ours_mask_grid_ok {
        return Err(err("our field grid differs from the registration grid"));
    }
    report.jacobian_forward_ours_mask = jacobian_stats(&our_fwd_field, Some(&mask))?;
    report.jacobian_forward_ants_mask =
        if ants_fwd_field.grid().approx_eq(template_iso.grid(), 1e-6) {
            jacobian_stats(&ants_fwd_field, Some(&mask))?
        } else {
            jacobian_stats(&ants_fwd_field, None)?
        };
    report.jacobian_forward_ours_all = jacobian_stats(&our_fwd_field, None)?;
    report.jacobian_forward_ants_all = jacobian_stats(&ants_fwd_field, None)?;
    report.jacobian_inverse_ours_mask = jacobian_stats(&our_inv_field, Some(&mask))?;
    report.jacobian_inverse_ants_mask =
        if ants_inv_field.grid().approx_eq(template_iso.grid(), 1e-6) {
            jacobian_stats(&ants_inv_field, Some(&mask))?
        } else {
            jacobian_stats(&ants_inv_field, None)?
        };

    let reco = read_reco_points(&files.reconstruction)?;
    let mut repro_all: Vec<f64> = Vec::new();
    let mut repro_pipeline: Vec<f64> = Vec::new();
    for (side, native, mni) in &reco.sides {
        let ants_mapped = map_ras(&ants_inv, native);
        let d = point_diff(side, &ants_mapped, mni);
        repro_all.extend(d.distances_mm.iter().cloned());
        let pipeline_ants = map_ras_pipeline_style(&ants_inv, native);
        repro_pipeline.extend(point_diff(side, &pipeline_ants, mni).distances_mm);
        report.points_vs_ants.push(point_diff(
            side,
            &map_ras_pipeline_style(&our_inv, native),
            mni,
        ));
        report.points_vs_ants_reader.push(point_diff(
            side,
            &map_ras(&our_inv, native),
            &ants_mapped,
        ));
    }
    report.ants_reproduction_max_mm = repro_all.iter().cloned().fold(0.0, f64::max);
    report.ants_reproduction_mean_mm =
        repro_all.iter().sum::<f64>() / repro_all.len().max(1) as f64;
    report.ants_reproduction_pipeline_max_mm = repro_pipeline.iter().cloned().fold(0.0, f64::max);
    report.ants_reproduction_pipeline_mean_mm =
        repro_pipeline.iter().sum::<f64>() / repro_pipeline.len().max(1) as f64;
    Ok(report)
}

/// Renders a report as text.
pub fn render(report: &AbReport) -> String {
    let mut s = String::new();
    let f = |d: &FieldDiff| {
        format!(
            "rms {:.4} mm, mean {:.4}, p95 {:.4}, max {:.4} (|ours| rms {:.3}, |ants| rms {:.3}, n {})",
            d.rms_mm, d.mean_mm, d.p95_mm, d.max_mm, d.a_rms_mm, d.b_rms_mm, d.voxels
        )
    };
    let j = |d: &JacobianStats| {
        format!(
            "min {:.4}, max {:.3}, mean {:.4}, {:.5} % <= 0 (n {})",
            d.min, d.max, d.mean, d.percent_non_positive, d.voxels
        )
    };
    s.push_str(&format!(
        "mask: {} ({} voxels)\n",
        report.mask_rule, report.mask_voxels
    ));
    let a = &report.affine_diff;
    s.push_str(&format!(
        "affine (element 0) vs ANTs over the mask: max {:.4} mm, mean {:.4}, rms {:.4}; matrix max |diff| {:.2e}; offset diff {:.4?} mm; det ours {:.5} ants {:.5}\n",
        a.max_mm,
        a.mean_mm,
        a.rms_mm,
        a.matrix_max_abs,
        a.offset_diff,
        a.det_ours,
        a.det_ants
    ));
    s.push_str(&format!(
        "forward field difference:  {}\n",
        f(&report.forward_diff)
    ));
    s.push_str(&format!(
        "inverse field difference:  {}\n",
        f(&report.inverse_diff)
    ));
    s.push_str(&format!(
        "jacobian forward ours (mask): {}\n",
        j(&report.jacobian_forward_ours_mask)
    ));
    s.push_str(&format!(
        "jacobian forward ants (mask): {}\n",
        j(&report.jacobian_forward_ants_mask)
    ));
    s.push_str(&format!(
        "jacobian forward ours (all):  {}\n",
        j(&report.jacobian_forward_ours_all)
    ));
    s.push_str(&format!(
        "jacobian forward ants (all):  {}\n",
        j(&report.jacobian_forward_ants_all)
    ));
    s.push_str(&format!(
        "jacobian inverse ours (mask): {}\n",
        j(&report.jacobian_inverse_ours_mask)
    ));
    s.push_str(&format!(
        "jacobian inverse ants (mask): {}\n",
        j(&report.jacobian_inverse_ants_mask)
    ));
    s.push_str(&format!(
        "ANTs InverseComposite reproduced by our reader: max {:.2e} mm, mean {:.2e} mm\n",
        report.ants_reproduction_max_mm, report.ants_reproduction_mean_mm
    ));
    s.push_str(&format!(
        "ANTs InverseComposite through the pipeline-style point warp vs stored mni: max {:.2e} mm, mean {:.2e} mm
",
        report.ants_reproduction_pipeline_max_mm, report.ants_reproduction_pipeline_mean_mm
    ));
    for p in &report.points_vs_ants {
        s.push_str(&format!(
            "points ours (pipeline-style) vs stored ANTs mni, side {}: n {}, mean {:.4} mm, max {:.4} mm\n",
            p.side, p.points, p.mean_mm, p.max_mm
        ));
    }
    for p in &report.points_vs_ants_reader {
        s.push_str(&format!(
            "points ours vs ANTs via our reader, side {}: n {}, mean {:.4} mm, max {:.4} mm\n",
            p.side, p.points, p.mean_mm, p.max_mm
        ));
    }
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use fastwarp_reg::linalg::IDENTITY3;

    fn grid() -> Grid {
        Grid::new([8, 7, 6], [1.0, 1.5, 2.0], [3.0, -2.0, 1.0], IDENTITY3).unwrap()
    }

    #[test]
    fn jacobian_of_a_uniform_stretch_is_its_determinant() {
        let g = grid();
        let f = DisplacementField::from_fn(g, |p| [0.1 * p[0], -0.2 * p[1], 0.0]);
        let s = jacobian_stats(&f, None).unwrap();
        assert!((s.min - 1.1 * 0.8).abs() < 1e-5, "{}", s.min);
        assert!((s.max - 1.1 * 0.8).abs() < 1e-5);
        assert_eq!(s.percent_non_positive, 0.0);
    }

    #[test]
    fn identical_fields_differ_by_zero_and_a_shift_by_its_length() {
        let g = grid();
        let a = DisplacementField::from_fn(g, |p| [p[0] * 0.01, 0.0, 0.0]);
        let b = DisplacementField::from_fn(g, |p| [p[0] * 0.01 + 0.3, 0.4, 0.0]);
        let mask = vec![true; g.num_voxels()];
        let same = compare_fields(&a, &a, &mask).unwrap();
        assert_eq!(same.max_mm, 0.0);
        let d = compare_fields(&a, &b, &mask).unwrap();
        assert!((d.rms_mm - 0.5).abs() < 1e-6 && (d.p95_mm - 0.5).abs() < 1e-6);
    }

    #[test]
    fn pipeline_style_point_mapping_is_exact_for_an_affine_chain() {
        let chain = TransformChain::from_queue(vec![TransformElem::Affine(
            AffineTransform::from_translation([3.0, -2.0, 5.0]),
        )]);
        let pts = [[1.25, -7.5, 3.125]];
        let direct = map_ras(&chain, &pts);
        let lattice = map_ras_pipeline_style(&chain, &pts);
        for a in 0..3 {
            assert!((direct[0][a] - lattice[0][a]).abs() < 1e-5);
        }
    }

    #[test]
    fn brain_mask_keeps_voxels_above_a_tenth_of_the_99th_percentile() {
        let g = grid();
        let data = (0..g.num_voxels())
            .map(|i| if i % 2 == 0 { 100.0 } else { 1.0 })
            .collect();
        let v = Volume::from_vec(g, data).unwrap();
        let m = brain_mask(&v, BRAIN_MASK_FRACTION);
        assert_eq!(m.iter().filter(|x| **x).count(), g.num_voxels() / 2);
    }
}
