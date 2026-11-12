// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use ndarray::{Array3, Array4};
use thiserror::Error;

use dbsloc_reconstruct::{py_list, sampling::trilinear_sample_vec};
use fastwarp_core::Mat4;

use crate::bids::simplify_verbatim;
use crate::nifti_io::{load_vector_field_f64, read_grid, write_f32_4d, write_u8_3d, NiftiIoError};

pub const GRID_MM: f64 = 1.0;
pub const GRID_PAD_MM: f64 = 10.0;
pub const COORD_GRID_MM: f64 = 8.0;
pub const COORD_PAD_MM: f64 = 120.0;

#[derive(Debug, Error)]
pub enum PointWarpError {
    #[error("points_ras_nx3 must be finite")]
    NotFinite,
    #[error("antsApplyTransforms not found (bundled or PATH)")]
    BinaryMissing,
    #[error("required file missing: {0}")]
    FileMissing(PathBuf),
    #[error("antsApplyTransforms failed (code {code}): {stderr}")]
    AntsFailed { code: i32, stderr: String },
    #[error("antsApplyTransforms returned shape {got:?}, expected {expected:?}")]
    Shape {
        got: Vec<usize>,
        expected: Vec<usize>,
    },
    #[error(
        "{n} point(s) mapped outside the coordinate image ({lo} to {hi}); the transform does not \
         fit these points and this reference"
    )]
    OutsideCoordinateImage { n: usize, lo: String, hi: String },
    #[error("{0}")]
    Nifti(#[from] NiftiIoError),
    #[error("{0}")]
    Io(String),
    #[error("cancelled by user")]
    Cancelled,
    #[error("{0}")]
    BadPoint(String),
    #[error("point warp returned {got} point(s) for {expected}")]
    Count { got: usize, expected: usize },
}

impl PointWarpError {
    pub fn python_kind(&self) -> &'static str {
        match self {
            PointWarpError::NotFinite | PointWarpError::BadPoint(_) => "ValueError",
            PointWarpError::BinaryMissing | PointWarpError::FileMissing(_) => "FileNotFoundError",
            _ => "RuntimeError",
        }
    }
}

pub trait PointWarp {
    fn to_template(&self, pts: &[[f64; 3]]) -> Result<Vec<[f64; 3]>, PointWarpError>;
}

fn resolved(p: &Path) -> String {
    let abs = std::fs::canonicalize(p)
        .map(simplify_verbatim)
        .unwrap_or_else(|_| p.to_path_buf());
    abs.to_string_lossy().into_owned()
}

pub fn composite_point_transform_tokens(composite: &Path) -> Option<Vec<String>> {
    composite
        .is_file()
        .then(|| vec!["-t".to_owned(), resolved(composite)])
}

fn scratch_dir() -> Result<PathBuf, PointWarpError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let d = std::env::temp_dir().join(format!(
        "dbsloc_pointwarp_{}_{}_{}",
        std::process::id(),
        nanos,
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&d).map_err(|e| PointWarpError::Io(format!("{}: {e}", d.display())))?;
    Ok(d)
}

fn diag_affine(spacing: f64, lo: [f64; 3]) -> Mat4 {
    [
        [spacing, 0.0, 0.0, lo[0]],
        [0.0, spacing, 0.0, lo[1]],
        [0.0, 0.0, spacing, lo[2]],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn coordinate_image(lo: [f64; 3], hi: [f64; 3], spacing: f64) -> (Mat4, Array4<f32>) {
    let mut n = [0usize; 3];
    for a in 0..3 {
        n[a] = (((hi[a] - lo[a]) / spacing).ceil() as i64 + 1).max(2) as usize;
    }
    let coords = Array4::from_shape_fn((n[0], n[1], n[2], 3), |(i, j, k, c)| {
        let idx = [i, j, k][c] as f64;
        (lo[c] + spacing * idx) as f32
    });
    (diag_affine(spacing, lo), coords)
}

fn grid_around(pts: &[[f64; 3]], spacing: f64, pad: f64) -> (Mat4, [usize; 3]) {
    let mut lo = [0.0f64; 3];
    let mut n = [0usize; 3];
    for a in 0..3 {
        let mn = pts.iter().map(|p| p[a]).fold(f64::INFINITY, f64::min);
        let mx = pts.iter().map(|p| p[a]).fold(f64::NEG_INFINITY, f64::max);
        lo[a] = ((mn - pad) / spacing).floor() * spacing;
        let hi = mx + pad;
        n[a] = (((hi - lo[a]) / spacing).ceil() as i64 + 1).max(2) as usize;
    }
    (diag_affine(spacing, lo), n)
}

fn drain<R: Read + Send + 'static>(r: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut r) = r {
            let _ = r.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

fn run_cancellable(
    cmd: &mut Command,
    exe: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<(Option<i32>, String, String), PointWarpError> {
    let (mut shortened, _aliases) = crate::outputs::shorten_long_inputs(cmd);
    let mut child = shortened
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PointWarpError::Io(format!("{}: {e}", exe.display())))?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let status = loop {
        if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out.join();
            let _ = err.join();
            return Err(PointWarpError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out.join();
                let _ = err.join();
                return Err(PointWarpError::Io(format!("{}: {e}", exe.display())));
            }
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    Ok((status.code(), stdout, stderr))
}

pub(crate) fn warp_points_via_antsapply_images(
    pts: &[[f64; 3]],
    dst_ref: &Path,
    transform_tokens: &[String],
    exe: Option<&Path>,
    threads: Option<u32>,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<[f64; 3]>, PointWarpError> {
    if pts.is_empty() {
        return Ok(Vec::new());
    }
    if pts.iter().any(|p| p.iter().any(|v| !v.is_finite())) {
        return Err(PointWarpError::NotFinite);
    }
    let exe = exe.ok_or(PointWarpError::BinaryMissing)?;
    if !dst_ref.is_file() {
        return Err(PointWarpError::FileMissing(dst_ref.to_path_buf()));
    }
    let grid = read_grid(dst_ref)?;
    let b = dbsloc_reconstruct::WorldBounds::from_grid(
        &grid.affine,
        grid.dims[0],
        grid.dims[1],
        grid.dims[2],
        0.0,
    );
    let lo_c = [
        b.min[0] - COORD_PAD_MM,
        b.min[1] - COORD_PAD_MM,
        b.min[2] - COORD_PAD_MM,
    ];
    let hi_c = [
        b.max[0] + COORD_PAD_MM,
        b.max[1] + COORD_PAD_MM,
        b.max[2] + COORD_PAD_MM,
    ];
    let (coord_aff, coord) = coordinate_image(lo_c, hi_c, COORD_GRID_MM);
    let (grid_aff, grid_shape) = grid_around(pts, GRID_MM, GRID_PAD_MM);

    let td = scratch_dir()?;
    let result = (|| {
        let coord_nii = td.join("coords.nii.gz");
        let grid_nii = td.join("grid.nii.gz");
        let out_nii = td.join("warped_coords.nii.gz");
        write_f32_4d(&coord_nii, &coord, &coord_aff)?;
        write_u8_3d(&grid_nii, &Array3::<u8>::zeros(grid_shape), &grid_aff)?;
        let mut cmd = Command::new(exe);
        cmd.arg("-d")
            .arg("3")
            .arg("-e")
            .arg("3")
            .arg("-i")
            .arg(&coord_nii)
            .arg("-r")
            .arg(&grid_nii)
            .arg("-o")
            .arg(&out_nii)
            .arg("-n")
            .arg("Linear")
            .args(transform_tokens);
        if let Some(n) = threads {
            cmd.env("ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS", n.max(1).to_string());
        }
        let (code, stdout, stderr) = run_cancellable(&mut cmd, exe, cancel)?;
        if code != Some(0) {
            let mut err = stderr.trim().to_owned();
            if err.is_empty() {
                err = stdout.trim().to_owned();
            }
            let err: String = err.chars().take(2000).collect();
            return Err(PointWarpError::AntsFailed {
                code: code.unwrap_or(-1),
                stderr: err,
            });
        }
        load_vector_field_f64(&out_nii).map_err(PointWarpError::from)
    })();
    let _ = std::fs::remove_dir_all(&td);
    let field = match result {
        Ok(f) => f,
        Err(PointWarpError::Nifti(NiftiIoError::NotThreeD { ndim, .. })) => {
            return Err(PointWarpError::Shape {
                got: vec![ndim],
                expected: vec![grid_shape[0], grid_shape[1], grid_shape[2], 3],
            })
        }
        Err(e) => return Err(e),
    };
    let fd = field.dim();
    if fd.3 != 3 || [fd.0, fd.1, fd.2] != grid_shape {
        return Err(PointWarpError::Shape {
            got: vec![fd.0, fd.1, fd.2, fd.3],
            expected: vec![grid_shape[0], grid_shape[1], grid_shape[2], 3],
        });
    }
    let lo_g = [grid_aff[0][3], grid_aff[1][3], grid_aff[2][3]];
    let mut out = Vec::with_capacity(pts.len());
    let mut dead = 0usize;
    for p in pts {
        let ijk = [
            (p[0] - lo_g[0]) / GRID_MM,
            (p[1] - lo_g[1]) / GRID_MM,
            (p[2] - lo_g[2]) / GRID_MM,
        ];
        let v = trilinear_sample_vec(&field, ijk);
        if v.iter().all(|x| *x == 0.0) {
            dead += 1;
        }
        out.push([v[0], v[1], v[2]]);
    }
    if dead > 0 {
        return Err(PointWarpError::OutsideCoordinateImage {
            n: dead,
            lo: py_list(&lo_c, Some(1)),
            hi: py_list(&hi_c, Some(1)),
        });
    }
    Ok(out)
}

/// Native-to-template point mapping in Rust: the chain of the inverse composite read by
/// `fastwarp-reg` and evaluated with [`fastwarp_reg::map_points_ras_lattice`], which reproduces the
/// coordinate-image route of [`warp_points_via_antsapply_images`] (same lattice, same `f32`
/// storage, same trilinear blend) without an `antsApplyTransforms` process.
#[derive(Clone, Debug)]
pub struct RustPointWarp {
    chain: fastwarp_reg::TransformChain,
}

impl RustPointWarp {
    /// Reads the native-to-template composite `file`.
    pub fn from_file(file: &Path) -> Result<RustPointWarp, PointWarpError> {
        let chain = fastwarp_reg::transform::read_transform_file(file)
            .map_err(|e| PointWarpError::Io(format!("{}: {e}", file.display())))?;
        Ok(RustPointWarp { chain })
    }

    /// Wraps an already-read native-to-template chain.
    pub fn from_chain(chain: fastwarp_reg::TransformChain) -> RustPointWarp {
        RustPointWarp { chain }
    }
}

impl PointWarp for RustPointWarp {
    fn to_template(&self, pts: &[[f64; 3]]) -> Result<Vec<[f64; 3]>, PointWarpError> {
        if pts.iter().any(|p| p.iter().any(|v| !v.is_finite())) {
            return Err(PointWarpError::NotFinite);
        }
        let out = fastwarp_reg::map_points_ras_lattice(&self.chain, pts);
        if out.iter().any(|p| p.iter().any(|v| !v.is_finite())) {
            return Err(PointWarpError::BadPoint(
                "the transform mapped a point to a non-finite position".into(),
            ));
        }
        Ok(out)
    }
}

#[derive(Clone, Debug)]
pub struct AntsPointWarp<'a> {
    pub exe: Option<PathBuf>,
    pub template: PathBuf,
    pub tokens: Vec<String>,
    pub threads: Option<u32>,
    pub cancel: Option<&'a AtomicBool>,
}

impl PointWarp for AntsPointWarp<'_> {
    fn to_template(&self, pts: &[[f64; 3]]) -> Result<Vec<[f64; 3]>, PointWarpError> {
        warp_points_via_antsapply_images(
            pts,
            &self.template,
            &self.tokens,
            self.exe.as_deref(),
            self.threads,
            self.cancel,
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use fastwarp_niftiio::WriterOptions;
    use nifti::NiftiHeader;

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("dbsloc_point_warp_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_composite_token_needs_the_file() {
        let d = tmp("for");
        let f = d.join("sub-01_from-MNI_to-anchorNative_mode-image_xfm.h5");
        assert!(composite_point_transform_tokens(&f).is_none());
        std::fs::write(&f, b"").unwrap();
        let t = composite_point_transform_tokens(&f).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0], "-t");
        assert!(t[1].ends_with("mode-image_xfm.h5"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn grid_and_coordinate_image_follow_the_numpy_layout() {
        let (aff, n) = grid_around(&[[1.0, 2.0, 3.0], [-10.5, 5.0, -7.0]], 1.0, 10.0);
        assert_eq!([aff[0][3], aff[1][3], aff[2][3]], [-21.0, -8.0, -17.0]);
        assert_eq!(n, [33, 24, 31]);
        let (caff, c) = coordinate_image([-10.0, 0.0, 5.0], [10.0, 4.0, 5.0], 8.0);
        assert_eq!(caff[0][0], 8.0);
        assert_eq!(c.dim(), (4, 2, 2, 3));
        assert_eq!(c[[3, 1, 0, 0]], 14.0);
        assert_eq!(c[[3, 1, 0, 1]], 8.0);
        assert_eq!(c[[3, 1, 1, 2]], 13.0);
    }

    #[test]
    fn non_finite_points_are_rejected_before_ants() {
        let e = warp_points_via_antsapply_images(
            &[[f64::NAN, 0.0, 0.0]],
            Path::new("x"),
            &[],
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(e, PointWarpError::NotFinite));
        assert_eq!(e.python_kind(), "ValueError");
        assert!(
            warp_points_via_antsapply_images(&[], Path::new("x"), &[], None, None, None)
                .unwrap()
                .is_empty()
        );
        let e =
            warp_points_via_antsapply_images(&[[0.0; 3]], Path::new("x"), &[], None, None, None)
                .unwrap_err();
        assert!(matches!(e, PointWarpError::BinaryMissing));
    }

    fn translation_field(path: &Path, d_lps: [f32; 3]) {
        let spacing = 4.0f32;
        let half = 40.0f32;
        let n = (2.0 * half / spacing) as usize + 1;
        let data = ndarray::Array5::from_shape_fn((n, n, n, 1, 3), |(_, _, _, _, c)| d_lps[c]);
        let hdr = NiftiHeader {
            sform_code: 2,
            qform_code: 0,
            srow_x: [spacing, 0.0, 0.0, -half],
            srow_y: [0.0, spacing, 0.0, -half],
            srow_z: [0.0, 0.0, spacing, -half],
            pixdim: [1.0, spacing, spacing, spacing, 1.0, 1.0, 1.0, 1.0],
            intent_code: 1007,
            ..NiftiHeader::default()
        };
        WriterOptions::new(path)
            .reference_header(&hdr)
            .write_nifti(&data)
            .unwrap();
    }

    #[test]
    fn a_set_cancel_flag_stops_the_call() {
        let Some(exe) = crate::bundled_bin::find_binary("antsApplyTransforms") else {
            return;
        };
        let d = tmp("cancel");
        let warp = d.join("warp.nii.gz");
        translation_field(&warp, [3.0, -2.0, 5.0]);
        let tokens = vec!["-t".to_owned(), warp.to_string_lossy().into_owned()];
        let cancel = AtomicBool::new(true);
        let e = warp_points_via_antsapply_images(
            &[[1.0, 2.0, 3.0]],
            &warp,
            &tokens,
            Some(&exe),
            Some(2),
            Some(&cancel),
        )
        .unwrap_err();
        assert!(matches!(e, PointWarpError::Cancelled));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn coordinate_image_method_matches_a_pure_translation() {
        let Some(exe) = crate::bundled_bin::find_binary("antsApplyTransforms") else {
            return;
        };
        let d = tmp("translate");
        let warp = d.join("warp.nii.gz");
        translation_field(&warp, [3.0, -2.0, 5.0]);
        let pts = [
            [1.0, 2.0, 3.0],
            [-10.0, 5.0, -7.0],
            [0.0, 0.0, 0.0],
            [6.5, -3.25, 9.0],
        ];
        let tokens = vec!["-t".to_owned(), warp.to_string_lossy().into_owned()];
        let got = warp_points_via_antsapply_images(&pts, &warp, &tokens, Some(&exe), Some(2), None)
            .unwrap();
        for (g, p) in got.iter().zip(pts.iter()) {
            let want = [p[0] - 3.0, p[1] + 2.0, p[2] + 5.0];
            for a in 0..3 {
                assert!((g[a] - want[a]).abs() < 0.05, "{g:?} vs {want:?}");
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    fn inverse_of(prefix: &Path) -> PathBuf {
        crate::derivatives::suffixed(prefix, crate::derivatives::ANTS_INVERSE_COMPOSITE_SUFFIX)
    }

    fn write_test_pair(prefix: &Path) -> fastwarp_reg::TransformChain {
        use fastwarp_reg::transform::{
            write_composite_pair, AffineTransform, CompositePair, DisplacementField,
            TransformChain, TransformElem,
        };
        use fastwarp_reg::Grid;
        let fgrid = Grid::from_ras_affine(
            [41, 41, 41],
            &[
                [4.0, 0.0, 0.0, -80.0],
                [0.0, 4.0, 0.0, -80.0],
                [0.0, 0.0, 4.0, -80.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        )
        .unwrap();
        let field = DisplacementField::from_fn(fgrid, |p| {
            [
                2.0 * (p[1] / 30.0).sin(),
                1.5 * (p[2] / 25.0).cos(),
                -(p[0] / 40.0).sin(),
            ]
        });
        let aff = AffineTransform::new(
            [[0.98, -0.05, 0.02], [0.05, 0.99, -0.03], [-0.02, 0.03, 1.0]],
            [3.0, -2.0, 1.5],
            [0.0; 3],
        );
        let inverse = TransformChain::from_queue(vec![
            TransformElem::DisplacementField(field),
            TransformElem::Affine(aff),
        ]);
        let pair = CompositePair {
            forward: TransformChain::from_queue(vec![TransformElem::Affine(aff)]),
            inverse: inverse.clone(),
        };
        write_composite_pair(prefix, &pair).unwrap();
        inverse
    }

    #[test]
    fn rust_route_matches_the_coordinate_image_route_run_through_the_cpu_apply() {
        use fastwarp_reg::apply::{apply_chain, Interpolation};
        use fastwarp_reg::{Grid, Volume};
        let d = tmp("rust_route");
        let prefix = d.join("sub-1_x_");
        write_test_pair(&prefix);
        let warp = RustPointWarp::from_file(&inverse_of(&prefix)).unwrap();
        let pts = [
            [1.25, -7.5, 3.75],
            [10.0, 20.0, -15.5],
            [-30.2, 4.4, 9.9],
            [0.0, 0.0, 0.0],
        ];
        let got = warp.to_template(&pts).unwrap();

        let chain = fastwarp_reg::transform::read_transform_file(&inverse_of(&prefix)).unwrap();
        let (coord_aff, coord) = coordinate_image([-120.0; 3], [120.0; 3], COORD_GRID_MM);
        let (cn0, cn1, cn2, _) = coord.dim();
        let cgrid = Grid::from_ras_affine([cn0, cn1, cn2], &coord_aff).unwrap();
        let (grid_aff, shape) = grid_around(&pts, GRID_MM, GRID_PAD_MM);
        let lgrid = Grid::from_ras_affine(shape, &grid_aff).unwrap();
        let mut components = Vec::new();
        for c in 0..3 {
            let mut data = vec![0.0f64; cgrid.num_voxels()];
            for k in 0..cn2 {
                for j in 0..cn1 {
                    for i in 0..cn0 {
                        data[cgrid.linear_index(i, j, k)] = f64::from(coord[[i, j, k, c]]);
                    }
                }
            }
            let vol = Volume::from_vec(cgrid, data).unwrap();
            components.push(apply_chain(&vol, &lgrid, &chain, Interpolation::Linear, 0.0).unwrap());
        }
        let field = Array4::from_shape_fn((shape[0], shape[1], shape[2], 3), |(i, j, k, c)| {
            components[c].data()[lgrid.linear_index(i, j, k)]
        });
        let lo = [grid_aff[0][3], grid_aff[1][3], grid_aff[2][3]];
        let mut worst = 0.0f64;
        for (p, g) in pts.iter().zip(&got) {
            let ijk = [
                (p[0] - lo[0]) / GRID_MM,
                (p[1] - lo[1]) / GRID_MM,
                (p[2] - lo[2]) / GRID_MM,
            ];
            let want = trilinear_sample_vec(&field, ijk);
            for a in 0..3 {
                worst = worst.max((g[a] - want[a]).abs());
            }
        }
        assert!(worst < 1e-4, "largest difference {worst} mm");
        let plain = fastwarp_reg::map_points_ras(&chain, &pts);
        for (g, q) in got.iter().zip(&plain) {
            for a in 0..3 {
                assert!((g[a] - q[a]).abs() < 0.05, "{g:?} {q:?}");
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rust_route_rejects_non_finite_points_and_unreadable_files() {
        let d = tmp("rust_route_err");
        let prefix = d.join("sub-1_x_");
        let file = inverse_of(&prefix);
        assert!(matches!(
            RustPointWarp::from_file(&file),
            Err(PointWarpError::Io(_))
        ));
        std::fs::write(&file, b"not hdf5").unwrap();
        assert!(matches!(
            RustPointWarp::from_file(&file),
            Err(PointWarpError::Io(_))
        ));
        let chain = fastwarp_reg::TransformChain::new();
        let w = RustPointWarp::from_chain(chain);
        assert!(matches!(
            w.to_template(&[[f64::NAN, 0.0, 0.0]]),
            Err(PointWarpError::NotFinite)
        ));
        assert!(w.to_template(&[]).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
