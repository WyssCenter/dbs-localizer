// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Output files of the `vat` stage (`specs/07` §5): names, uncompressed NIfTI writers, the
//! `efieldgauss` map and the per-hemisphere MAT file.

use std::path::{Path, PathBuf};

use fastwarp_core::Mat4;
use fastwarp_matfile::mat5::{mat5_bytes, Mat5Value};
use fastwarp_niftiio::WriterOptions;
use ndarray::Array3;
use nifti::NiftiHeader;

use crate::error::{PipelineError, PipelineResult};
use crate::vat::mesh::Mesh;
use crate::vat::settings::Side;

/// The product label written into the NIfTI `descrip` field.
pub const NIFTI_DESCRIP: &str = "dbsloc vta";

/// File names of one label folder for one model slug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    subject: String,
    slug: &'static str,
}

impl Names {
    pub fn new(subject: &str, slug: &'static str) -> Self {
        Names {
            subject: subject.to_owned(),
            slug,
        }
    }

    fn sim(&self, kind: &str, side: Side, ext: &str) -> String {
        format!(
            "sub-{}_sim-{kind}_model-{}_hemi-{}.{ext}",
            self.subject,
            self.slug,
            side.letter()
        )
    }

    pub fn binary_nii(&self, side: Side) -> String {
        self.sim("binary", side, "nii")
    }

    pub fn efield_nii(&self, side: Side) -> String {
        self.sim("efield", side, "nii")
    }

    pub fn efieldgauss_nii(&self, side: Side) -> String {
        self.sim("efieldgauss", side, "nii")
    }

    pub fn hemi_mat(&self, side: Side) -> String {
        self.sim("binary", side, "mat")
    }

    pub fn stimparameters_mat(&self) -> String {
        format!("sub-{}_desc-stimparameters.mat", self.subject)
    }

    pub fn vta_json(&self) -> String {
        format!("sub-{}_desc-vta.json", self.subject)
    }

    pub fn vtastats_json(&self) -> String {
        format!("sub-{}_desc-vtastats.json", self.subject)
    }

    /// Every file of this slug in a label folder that a run may write; the ones a run does not
    /// produce are removed at commit.
    pub fn owned_files(&self, with_stats: bool) -> Vec<String> {
        let mut v = vec![self.stimparameters_mat(), self.vta_json()];
        for side in Side::ALL {
            v.push(self.binary_nii(side));
            v.push(self.efield_nii(side));
            v.push(self.efieldgauss_nii(side));
            v.push(self.hemi_mat(side));
        }
        if with_stats {
            v.push(self.vtastats_json());
        }
        v
    }
}

/// NIfTI orientation codes copied from a reference image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Codes {
    pub qform: i16,
    pub sform: i16,
}

impl Default for Codes {
    fn default() -> Self {
        Codes { qform: 2, sform: 2 }
    }
}

impl Codes {
    /// The codes of `image`'s header; both 0 or an unreadable header give the default 2 / 2.
    pub fn of_image(image: Option<&Path>) -> Codes {
        let hdr = image.and_then(|p| NiftiHeader::from_file(p).ok());
        match hdr {
            Some(h) if h.qform_code != 0 || h.sform_code != 0 => Codes {
                qform: h.qform_code,
                sform: h.sform_code,
            },
            _ => Codes::default(),
        }
    }
}

fn header(aff: &Mat4, codes: Codes) -> NiftiHeader {
    let row = |r: usize| {
        [
            aff[r][0] as f32,
            aff[r][1] as f32,
            aff[r][2] as f32,
            aff[r][3] as f32,
        ]
    };
    let spacing = fastwarp_core::voxel_spacing_rowmajor(aff);
    let mut pixdim = [1.0f32; 8];
    for a in 0..3 {
        pixdim[a + 1] = spacing[a] as f32;
    }
    let mut descrip = NIFTI_DESCRIP.as_bytes().to_vec();
    descrip.resize(80, 0);
    NiftiHeader {
        pixdim,
        sform_code: codes.sform,
        qform_code: codes.qform,
        srow_x: row(0),
        srow_y: row(1),
        srow_z: row(2),
        quatern_b: 0.0,
        quatern_c: 0.0,
        quatern_d: 0.0,
        quatern_x: aff[0][3] as f32,
        quatern_y: aff[1][3] as f32,
        quatern_z: aff[2][3] as f32,
        xyzt_units: 2,
        descrip,
        ..NiftiHeader::default()
    }
}

fn io_error(path: &Path, e: impl std::fmt::Display) -> PipelineError {
    PipelineError::Io(format!("cannot write {}: {e}", path.display()))
}

fn make_parent(path: &Path) -> PipelineResult<()> {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => {
            std::fs::create_dir_all(p).map_err(|e| io_error(path, e))
        }
        _ => Ok(()),
    }
}

fn shaped<T: Copy>(dims: [usize; 3], data: &[T]) -> Array3<T> {
    Array3::from_shape_fn((dims[0], dims[1], dims[2]), |(i, j, k)| {
        data[i + dims[0] * (j + dims[1] * k)]
    })
}

/// Writes an uncompressed `int16` 0 / 1 image (the reference datatype of the binary VTA).
pub fn write_binary_nii(
    path: &Path,
    dims: [usize; 3],
    data: &[u8],
    aff: &Mat4,
    codes: Codes,
) -> PipelineResult<()> {
    let values: Vec<i16> = data.iter().map(|&v| i16::from(v != 0)).collect();
    make_parent(path)?;
    WriterOptions::new(path)
        .reference_header(&header(aff, codes))
        .write_nifti(&shaped(dims, &values))
        .map_err(|e| io_error(path, e))
}

/// Writes an uncompressed `float32` image.
pub fn write_f32_nii(
    path: &Path,
    dims: [usize; 3],
    data: &[f32],
    aff: &Mat4,
    codes: Codes,
) -> PipelineResult<()> {
    make_parent(path)?;
    WriterOptions::new(path)
        .reference_header(&header(aff, codes))
        .write_nifti(&shaped(dims, data))
        .map_err(|e| io_error(path, e))
}

fn erf_series(x: f64) -> f64 {
    let mut term = x;
    let mut sum = x;
    let mut n = 1.0;
    while n < 200.0 {
        term *= -x * x / n;
        let add = term / (2.0 * n + 1.0);
        sum += add;
        if add.abs() < 1e-17 * sum.abs() {
            break;
        }
        n += 1.0;
    }
    sum * 2.0 / std::f64::consts::PI.sqrt()
}

fn erfc(x: f64) -> f64 {
    if x < 0.0 {
        return 2.0 - erfc(-x);
    }
    if x < 2.5 {
        return 1.0 - erf_series(x);
    }
    let mut f = x;
    for k in (1..=60).rev() {
        f = x + (f64::from(k) / 2.0) / f;
    }
    (-x * x).exp() / (std::f64::consts::PI.sqrt() * f)
}

/// Standard normal cumulative distribution function.
pub fn normal_cdf(z: f64) -> f64 {
    0.5 * erfc(-z / std::f64::consts::SQRT_2)
}

/// Inverse of [`normal_cdf`] for `p` in (0, 1).
pub fn normal_quantile(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969683028665376e1,
        2.209460984245205e2,
        -2.759285104469687e2,
        1.383_577_518_672_69e2,
        -3.066479806614716e1,
        2.506628277459239,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e1,
        1.615858368580409e2,
        -1.556989798598866e2,
        6.680131188771972e1,
        -1.328068155288572e1,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-3,
        -3.223964580411365e-1,
        -2.400758277161838,
        -2.549732539343734,
        4.374664141464968,
        2.938163982698783,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-3,
        3.224671290700398e-1,
        2.445134137142996,
        3.754408661907416,
    ];
    let low = 0.02425;
    let mut x = if p < low {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - low {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    let e = normal_cdf(x) - p;
    let u = e * (2.0 * std::f64::consts::PI).sqrt() * (x * x / 2.0).exp();
    x -= u / (1.0 + x * u / 2.0);
    x
}

/// The `efieldgauss` map of `specs/07` §5.3: inside `mask`, the rank-based inverse-normal
/// transform of `e`, shifted so the smallest value is 0 and divided by the sum.
pub fn efield_gauss(e: &[f32], mask: &[u8]) -> Vec<f32> {
    let mut inside: Vec<usize> = (0..e.len()).filter(|&i| mask[i] != 0).collect();
    let mut out = vec![0f32; e.len()];
    let m = inside.len();
    if m == 0 {
        return out;
    }
    inside.sort_by(|&a, &b| e[a].total_cmp(&e[b]).then(a.cmp(&b)));
    let z: Vec<f64> = (1..=m)
        .map(|r| normal_quantile((r as f64 - 0.5) / m as f64))
        .collect();
    let zmin = z[0];
    let shifted: Vec<f64> = z.iter().map(|v| v - zmin).collect();
    let sum: f64 = shifted.iter().sum();
    if sum <= 0.0 {
        return out;
    }
    for (rank, &idx) in inside.iter().enumerate() {
        out[idx] = (shifted[rank] / sum) as f32;
    }
    out
}

/// A display arrow set of the `vatgrad` struct (positions and unit-scaled vectors in mm).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gradient {
    pub positions: Vec<[f64; 3]>,
    pub vectors: Vec<[f64; 3]>,
}

fn column(values: impl Iterator<Item = f64>) -> Mat5Value {
    let data: Vec<f64> = values.collect();
    Mat5Value::Double {
        dims: vec![data.len(), 1],
        data,
    }
}

fn named(fields: Vec<(&str, Mat5Value)>) -> Mat5Value {
    Mat5Value::struct_scalar(fields.into_iter().map(|(n, v)| (n.to_owned(), v)).collect())
}

/// The MAT v7 bytes of a hemisphere file: `vatfv`, `vatvolume` and optionally `vatgrad`.
pub fn hemi_mat_bytes(
    mesh: &Mesh,
    volume_mm3: f64,
    gradient: Option<&Gradient>,
) -> PipelineResult<Vec<u8>> {
    let faces: Vec<[f64; 3]> = mesh
        .faces
        .iter()
        .map(|f| {
            [
                f64::from(f[0]) + 1.0,
                f64::from(f[1]) + 1.0,
                f64::from(f[2]) + 1.0,
            ]
        })
        .collect();
    let mut vars = vec![
        (
            "vatfv".to_owned(),
            named(vec![
                ("faces", Mat5Value::rows3(&faces)),
                ("vertices", Mat5Value::rows3(&mesh.vertices)),
            ]),
        ),
        ("vatvolume".to_owned(), Mat5Value::scalar(volume_mm3)),
    ];
    if let Some(g) = gradient {
        vars.push((
            "vatgrad".to_owned(),
            named(vec![
                ("x", column(g.positions.iter().map(|p| p[0]))),
                ("y", column(g.positions.iter().map(|p| p[1]))),
                ("z", column(g.positions.iter().map(|p| p[2]))),
                ("qx", column(g.vectors.iter().map(|p| p[0]))),
                ("qy", column(g.vectors.iter().map(|p| p[1]))),
                ("qz", column(g.vectors.iter().map(|p| p[2]))),
            ]),
        ));
    }
    mat5_bytes(&vars, true).map_err(|e| PipelineError::Message(format!("vat: MAT file: {e}")))
}

/// Writes `bytes` to `path`, creating the folder.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> PipelineResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_error(path, e))?;
    }
    std::fs::write(path, bytes).map_err(|e| io_error(path, e))
}

/// The label folder of `space` under the subject's stimulation root.
pub fn space_dir(stim_root: &Path, space: &str, label: &str) -> PathBuf {
    stim_root.join(space).join(label)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_lead_dbs_pattern() {
        let n = Names::new("01", "simbio");
        assert_eq!(
            n.binary_nii(Side::Right),
            "sub-01_sim-binary_model-simbio_hemi-R.nii"
        );
        assert_eq!(
            n.efieldgauss_nii(Side::Left),
            "sub-01_sim-efieldgauss_model-simbio_hemi-L.nii"
        );
        assert_eq!(
            n.hemi_mat(Side::Left),
            "sub-01_sim-binary_model-simbio_hemi-L.mat"
        );
        assert_eq!(n.vta_json(), "sub-01_desc-vta.json");
    }

    #[test]
    fn the_normal_quantile_inverts_the_cdf() {
        assert!(normal_quantile(0.5).abs() < 1e-12);
        assert!((normal_quantile(0.975) - 1.959_963_984_540_054).abs() < 1e-9);
        assert!((normal_quantile(0.001) + 3.090_232_306_167_813_6).abs() < 1e-8);
        for p in [1e-6, 0.01, 0.3, 0.7, 0.99, 1.0 - 1e-6] {
            assert!((normal_cdf(normal_quantile(p)) - p).abs() < 1e-12, "{p}");
        }
    }

    #[test]
    fn efieldgauss_is_rank_based_non_negative_and_sums_to_one() {
        let e = [5.0f32, 1.0, 3.0, 9.0, 2.0, 7.0];
        let mask = [1u8, 1, 1, 0, 1, 1];
        let g = efield_gauss(&e, &mask);
        assert_eq!(g[3], 0.0);
        let sum: f32 = g.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6, "{sum}");
        assert_eq!(g[1], 0.0);
        assert!(g[4] > 0.0 && g[4] < g[2] && g[2] < g[0] && g[0] < g[5]);
        let again = efield_gauss(&e, &mask);
        assert_eq!(g, again);
    }

    #[test]
    fn nifti_files_are_uncompressed_with_the_given_affine() {
        let d = std::env::temp_dir().join(format!("dbsloc_vat_out_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let aff = [
            [0.5, 0.0, 0.0, -3.0],
            [0.0, 0.5, 0.0, 4.0],
            [0.0, 0.0, 0.5, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let p = d.join("b.nii");
        let data: Vec<u8> = (0..24).map(|i| u8::from(i % 3 == 0)).collect();
        write_binary_nii(&p, [2, 3, 4], &data, &aff, Codes::default()).unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert_ne!(&raw[0..2], &[0x1f, 0x8b]);
        let (vol, got) = crate::nifti_io::load_volume_f64(&p).unwrap();
        assert_eq!(vol.dim(), (2, 3, 4));
        assert!((got[0][3] + 3.0).abs() < 1e-6 && (got[0][0] - 0.5).abs() < 1e-6);
        assert_eq!(vol[[0, 0, 0]], 1.0);
        assert_eq!(vol[[1, 0, 0]], 0.0);
        assert_eq!(vol[[1, 1, 0]], 1.0);
        let q = d.join("f.nii");
        let f: Vec<f32> = (0..24).map(|i| i as f32).collect();
        write_f32_nii(&q, [2, 3, 4], &f, &aff, Codes::default()).unwrap();
        let (vol, _) = crate::nifti_io::load_volume_f64(&q).unwrap();
        assert_eq!(vol[[1, 2, 3]], 23.0);
        assert_eq!(vol[[1, 0, 0]], 1.0);
        let _ = std::fs::remove_dir_all(&d);
    }
}
