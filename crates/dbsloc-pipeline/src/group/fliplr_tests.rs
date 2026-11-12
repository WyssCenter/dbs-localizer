#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::time::Instant;

use fastwarp_core::Mat4;
use fastwarp_reg::{Grid, Volume};
use tempfile::TempDir;

use super::super::flip::{
    flip_nifti, flip_points_through, flipped_is_current, mirror_label_dir, mirrored_affine,
    ImageKind,
};
use super::super::record::subject_id;
use super::*;
use crate::nifti_io::load_volume_f64;
use crate::reg_backend::CpuReferenceEngine;
use crate::vat::outputs::{write_binary_nii, Codes};

const N: usize = 32;
const SPACE: &str = "Phantom";

fn affine() -> Mat4 {
    [
        [2.0, 0.0, 0.0, -31.0],
        [0.0, 2.0, 0.0, -31.0],
        [0.0, 0.0, 2.0, -31.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn world(i: usize, j: usize, k: usize) -> [f64; 3] {
    let a = affine();
    [
        a[0][0] * i as f64 + a[0][3],
        a[1][1] * j as f64 + a[1][3],
        a[2][2] * k as f64 + a[2][3],
    ]
}

fn inside(p: [f64; 3], c: [f64; 3], r: [f64; 3]) -> bool {
    (0..3).map(|a| ((p[a] - c[a]) / r[a]).powi(2)).sum::<f64>() <= 1.0
}

fn phantom(invert: bool) -> Vec<f32> {
    let mut data = Vec::with_capacity(N * N * N);
    for k in 0..N {
        for j in 0..N {
            for i in 0..N {
                let p = world(i, j, k);
                let mut v = 0.0;
                if inside(p, [0.0; 3], [26.0; 3]) {
                    v = 0.2;
                }
                if inside(p, [16.0, 0.0, 0.0], [8.0, 4.0, 4.0]) {
                    v = 1.0;
                }
                if inside(p, [-16.0, 0.0, 0.0], [5.0; 3]) {
                    v = 0.5;
                }
                data.push(if invert { 1.0 - v } else { v });
            }
        }
    }
    data
}

fn write_template(path: &Path, invert: bool) {
    let grid = Grid::from_ras_affine([N, N, N], &affine()).unwrap();
    Volume::from_vec(grid, phantom(invert))
        .unwrap()
        .save_nifti(path)
        .unwrap();
}

struct Built {
    _dir: TempDir,
    root: PathBuf,
    paths: FlipPaths,
}

fn built() -> Built {
    let dir = tempfile::Builder::new()
        .prefix("dbsloc_fliplr_")
        .tempdir()
        .unwrap();
    let root = dir.path().join("userdata");
    let tdir = space_dir_in(&root, SPACE).join("res-2mm");
    std::fs::create_dir_all(&tdir).unwrap();
    write_template(&tdir.join("t1.nii.gz"), false);
    write_template(&tdir.join("t2.nii.gz"), true);
    let files = TemplateFiles::from_t1(SPACE, &tdir.join("t1.nii.gz"));
    assert!(files.t2.is_some());
    let out = cache_dir_in(&root, SPACE);
    let t = Instant::now();
    let paths = build_with(&CpuReferenceEngine, &files, &out, None, None, None).unwrap();
    eprintln!(
        "phantom flip transform built in {:.1} s",
        t.elapsed().as_secs_f64()
    );
    Built {
        _dir: dir,
        root,
        paths,
    }
}

fn sphere(centre: [f64; 3], radius: f64) -> Vec<u8> {
    let mut v = Vec::with_capacity(N * N * N);
    for k in 0..N {
        for j in 0..N {
            for i in 0..N {
                v.push(u8::from(inside(world(i, j, k), centre, [radius; 3])));
            }
        }
    }
    v
}

fn dice(a: &[f64], b: &[f64]) -> f64 {
    let (mut both, mut na, mut nb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(u8::from(*x > 0.5)), f64::from(u8::from(*y > 0.5)));
        both += x * y;
        na += x;
        nb += y;
    }
    2.0 * both / (na + nb)
}

fn centroid_x(data: &[f64]) -> f64 {
    let (mut sum, mut n) = (0.0, 0.0);
    for k in 0..N {
        for j in 0..N {
            for i in 0..N {
                if data[i + N * (j + N * k)] > 0.5 {
                    sum += world(i, j, k)[0];
                    n += 1.0;
                }
            }
        }
    }
    sum / n
}

fn flat(path: &Path) -> (Vec<f64>, Mat4) {
    let (vol, aff) = load_volume_f64(path).unwrap();
    let mut out = Vec::new();
    for k in 0..N {
        for j in 0..N {
            for i in 0..N {
                out.push(vol[[i, j, k]]);
            }
        }
    }
    (out, aff)
}

#[test]
fn phantom_transform_mirrors_volumes_and_points() {
    let b = built();
    assert!(b.paths.forward.is_file() && b.paths.inverse.is_file());
    assert_eq!(
        b.paths.forward.file_name().unwrap().to_string_lossy(),
        "from-Phantom_to-PhantomFlipped_mode-image_xfm.h5"
    );
    assert_eq!(
        b.paths.inverse.file_name().unwrap().to_string_lossy(),
        "from-PhantomFlipped_to-Phantom_mode-image_xfm.h5"
    );
    let record = read_record(&cache_dir_in(&b.root, SPACE)).unwrap();
    assert_eq!(record["templates"].as_array().unwrap().len(), 2);
    assert_eq!(record["space"], SPACE);
    assert!(record["software"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(record["engine"]["name"].is_string());
    let leftovers: Vec<String> = std::fs::read_dir(cache_dir_in(&b.root, SPACE))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers.len(), 3, "{leftovers:?}");

    let tmp = tempfile::tempdir().unwrap();
    let centre = [6.0, 14.0, 8.0];
    let src = tmp.path().join("sphere.nii");
    write_binary_nii(
        &src,
        [N, N, N],
        &sphere(centre, 5.0),
        &affine(),
        Codes::default(),
    )
    .unwrap();
    let once = tmp.path().join("once.nii");
    let twice = tmp.path().join("twice.nii");
    flip_nifti(&src, &once, ImageKind::Binary, Some(&b.paths.forward)).unwrap();
    flip_nifti(&once, &twice, ImageKind::Binary, Some(&b.paths.forward)).unwrap();
    let (orig, _) = flat(&src);
    let (mid, mid_aff) = flat(&once);
    let (back, _) = flat(&twice);
    assert_eq!(mid_aff, affine());
    assert!(
        (centroid_x(&mid) + centre[0]).abs() <= 2.0,
        "{}",
        centroid_x(&mid)
    );
    let d = dice(&orig, &back);
    assert!(d >= 0.9, "double flip Dice {d}");

    let pts = [[6.0, 14.0, 8.0], [-20.0, -5.0, 3.0], [12.0, 0.0, 0.0]];
    let flipped = flip_points_through(&pts, &b.paths.forward).unwrap();
    for (p, f) in pts.iter().zip(&flipped) {
        assert!((p[0] + f[0]).abs() <= 2.0, "{p:?} -> {f:?}");
        assert!((p[1] - f[1]).abs() <= 2.0 && (p[2] - f[2]).abs() <= 2.0);
    }

    let efield = tmp.path().join("efield.nii");
    let values: Vec<f32> = sphere(centre, 5.0)
        .iter()
        .map(|&v| f32::from(v) * 250.0)
        .collect();
    Volume::from_vec(Grid::from_ras_affine([N, N, N], &affine()).unwrap(), values)
        .unwrap()
        .save_nifti(&efield)
        .unwrap();
    let out = tmp.path().join("efield_flipped.nii");
    flip_nifti(&efield, &out, ImageKind::Float, Some(&b.paths.forward)).unwrap();
    let (vals, _) = flat(&out);
    let max = vals.iter().copied().fold(0.0, f64::max);
    assert!(max > 200.0 && max <= 250.5, "{max}");
}

const OFF_NX: usize = 12;

fn off_centre_affine() -> Mat4 {
    [
        [2.0, 0.0, 0.0, 1.0],
        [0.0, 2.0, 0.0, -31.0],
        [0.0, 0.0, 2.0, -31.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn off_centre_sphere(centre: [f64; 3], radius: f64) -> Vec<u8> {
    let a = off_centre_affine();
    let mut v = Vec::with_capacity(OFF_NX * N * N);
    for k in 0..N {
        for j in 0..N {
            for i in 0..OFF_NX {
                let p = [
                    a[0][0] * i as f64 + a[0][3],
                    a[1][1] * j as f64 + a[1][3],
                    a[2][2] * k as f64 + a[2][3],
                ];
                v.push(u8::from(inside(p, centre, [radius; 3])));
            }
        }
    }
    v
}

fn read_any(path: &Path) -> (Vec<f64>, Mat4, [usize; 3]) {
    let (vol, aff) = load_volume_f64(path).unwrap();
    let (nx, ny, nz) = vol.dim();
    let mut out = Vec::with_capacity(nx * ny * nz);
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                out.push(vol[[i, j, k]]);
            }
        }
    }
    (out, aff, [nx, ny, nz])
}

fn centroid_x_on(data: &[f64], dims: [usize; 3], aff: &Mat4) -> f64 {
    let (mut sum, mut n) = (0.0, 0.0);
    for k in 0..dims[2] {
        for j in 0..dims[1] {
            for i in 0..dims[0] {
                if data[i + dims[0] * (j + dims[1] * k)] > 0.5 {
                    sum += fastwarp_core::apply_rowmajor(aff, [i as f64, j as f64, k as f64])[0];
                    n += 1.0;
                }
            }
        }
    }
    sum / n
}

#[test]
fn an_off_centre_grid_is_mirrored_through_the_transform_onto_the_mirrored_grid() {
    let b = built();
    let tmp = tempfile::tempdir().unwrap();
    let centre = [9.0, 14.0, 8.0];
    let src = tmp.path().join("off.nii");
    write_binary_nii(
        &src,
        [OFF_NX, N, N],
        &off_centre_sphere(centre, 5.0),
        &off_centre_affine(),
        Codes::default(),
    )
    .unwrap();
    let once = tmp.path().join("off_once.nii");
    let twice = tmp.path().join("off_twice.nii");
    flip_nifti(&src, &once, ImageKind::Binary, Some(&b.paths.forward)).unwrap();
    flip_nifti(&once, &twice, ImageKind::Binary, Some(&b.paths.forward)).unwrap();

    let (orig, orig_aff, dims) = read_any(&src);
    let (mid, mid_aff, mid_dims) = read_any(&once);
    let (back, back_aff, back_dims) = read_any(&twice);
    assert_eq!(mid_dims, dims);
    let hits = mid.iter().filter(|&&v| v > 0.5).count();
    assert!(hits > 0, "the mirrored image is empty");
    let mirrored_origin = -(off_centre_affine()[0][0] * (OFF_NX as f64 - 1.0) + 1.0);
    assert_eq!(mid_aff[0][3], mirrored_origin);
    let cx = centroid_x_on(&mid, mid_dims, &mid_aff);
    assert!((cx + centre[0]).abs() <= 2.0, "centroid x {cx}");

    assert_eq!(back_dims, dims);
    assert_eq!(back_aff, orig_aff);
    let d = dice(&orig, &back);
    assert!(d >= 0.9, "double flip Dice {d}");
    assert!(flipped_is_current(&src, &once));

    let efield = tmp.path().join("off_efield.nii");
    let values: Vec<f32> = off_centre_sphere(centre, 5.0)
        .iter()
        .map(|&v| f32::from(v) * 250.0)
        .collect();
    Volume::from_vec(
        Grid::from_ras_affine([OFF_NX, N, N], &off_centre_affine()).unwrap(),
        values,
    )
    .unwrap()
    .save_nifti(&efield)
    .unwrap();
    let out = tmp.path().join("off_efield_flipped.nii");
    flip_nifti(&efield, &out, ImageKind::Float, Some(&b.paths.forward)).unwrap();
    let (vals, aff, vdims) = read_any(&out);
    assert_eq!(aff[0][3], mirrored_origin);
    let max = vals.iter().copied().fold(0.0, f64::max);
    assert!(max > 200.0 && max <= 250.5, "{max}");
    let high: Vec<f64> = vals
        .iter()
        .map(|&v| f64::from(u8::from(v > 125.0)))
        .collect();
    let cx = centroid_x_on(&high, vdims, &aff);
    assert!((cx + centre[0]).abs() <= 2.0, "E-field centroid x {cx}");
    assert!(flipped_is_current(&efield, &out));
}

#[test]
fn flipping_a_label_folder_keeps_the_lead_dbs_names_and_writes_the_mirrored_grid() {
    let b = built();
    for off_centre in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (dims, aff) = if off_centre {
            ([OFF_NX, N, N], off_centre_affine())
        } else {
            ([N, N, N], affine())
        };
        let ball = |c: [f64; 3]| {
            if off_centre {
                off_centre_sphere(c, 4.0)
            } else {
                sphere(c, 4.0)
            }
        };
        for (kind, side) in [("binary", "R"), ("efield", "L")] {
            let name = format!("sub-01_sim-{kind}_model-kuncel_hemi-{side}.nii");
            let path = dir.path().join(name);
            if kind == "binary" {
                write_binary_nii(&path, dims, &ball([8.0, 10.0, 4.0]), &aff, Codes::default())
                    .unwrap();
            } else {
                let grid = Grid::from_ras_affine(dims, &aff).unwrap();
                let centre_x = if off_centre { 12.0 } else { -8.0 };
                let v: Vec<f32> = ball([centre_x, 10.0, 4.0])
                    .iter()
                    .map(|&x| f32::from(x))
                    .collect();
                Volume::from_vec(grid, v)
                    .unwrap()
                    .save_nifti(&path)
                    .unwrap();
            }
        }
        let written = mirror_label_dir(
            dir.path(),
            subject_id("sub-01"),
            "kuncel",
            Some(&b.paths.forward),
        )
        .unwrap();
        let names: Vec<String> = written
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "sub-01_sim-binary_model-kuncel_hemi-L_hemidesc-FlippedFromRight.nii",
                "sub-01_sim-efield_model-kuncel_hemi-R_hemidesc-FlippedFromLeft.nii",
            ]
        );
        let want = mirrored_affine(dims, &aff);
        assert_eq!(want == aff, !off_centre);
        for p in &written {
            let (data, got, got_dims) = read_any(p);
            assert_eq!(got_dims, dims);
            assert_eq!(got, want, "off centre {off_centre}");
            assert!(
                data.iter().any(|&v| v > 0.0),
                "off centre {off_centre}: {p:?}"
            );
        }
    }
}

#[test]
fn lookup_prefers_the_user_directory_and_a_stale_build_is_not_used() {
    let tmp = tempfile::tempdir().unwrap();
    let user = tmp.path().join("user");
    let tdir = tmp.path().join("shipped").join("res-2mm");
    std::fs::create_dir_all(&tdir).unwrap();
    std::fs::write(tdir.join("t1.nii.gz"), b"template one").unwrap();
    let files = TemplateFiles::from_t1(SPACE, &tdir.join("t1.nii.gz"));
    let shipped = shipped_dirs(SPACE, Some(&files.t1), None);
    assert_eq!(shipped[0], tdir.join(DIR_NAME));

    assert!(locate(SPACE, Some(&files), Some(&user), &shipped).is_none());

    let user_dir = cache_dir_in(&user, SPACE);
    let user_paths = paths_in(&user_dir, SPACE);
    std::fs::create_dir_all(&user_dir).unwrap();
    std::fs::write(&user_paths.forward, b"f").unwrap();
    std::fs::write(&user_paths.inverse, b"i").unwrap();
    assert_eq!(
        locate(SPACE, Some(&files), Some(&user), &shipped),
        Some(user_paths.clone()),
        "no build.json: placed by hand, accepted"
    );

    let record = json!({"templates": files.fingerprint().unwrap()});
    std::fs::write(user_dir.join(BUILD_RECORD), record.to_string()).unwrap();
    assert_eq!(
        locate(SPACE, Some(&files), Some(&user), &shipped),
        Some(user_paths.clone())
    );
    let env = FlipEnv {
        user_root: Some(user.clone()),
        shipped_root: None,
        backend: RegBackend::Auto,
        threads: None,
    };
    assert!(!describe(&files, &env).stale);

    std::fs::write(&files.t1, b"template two").unwrap();
    assert!(locate(SPACE, Some(&files), Some(&user), &shipped).is_none());
    let status = describe(&files, &env);
    assert!(status.stale && status.found.is_none());

    let shipped_paths = paths_in(&shipped[0], SPACE);
    std::fs::create_dir_all(&shipped[0]).unwrap();
    std::fs::write(&shipped_paths.forward, b"f").unwrap();
    std::fs::write(&shipped_paths.inverse, b"i").unwrap();
    assert_eq!(
        locate(SPACE, Some(&files), Some(&user), &shipped),
        Some(shipped_paths)
    );
}

#[test]
fn the_inverse_is_found_by_name() {
    let f = Path::new("x/from-MNI_to-MNIFlipped_mode-image_xfm.h5");
    assert_eq!(
        inverse_of(f).unwrap(),
        Path::new("x/from-MNIFlipped_to-MNI_mode-image_xfm.h5")
    );
    assert_eq!(
        inverse_of(Path::new("y/flip_Composite.h5")).unwrap(),
        Path::new("y/flip_InverseComposite.h5")
    );
    assert!(inverse_of(Path::new("y/flip_InverseComposite.h5")).is_none());
    assert!(inverse_of(Path::new("y/Composite.nii.gz")).is_none());
}

#[test]
#[ignore]
fn real_template_flip_round_trip() {
    use super::super::flip::{flip_points, flip_points_through};
    let env = FlipEnv::from_environment(RegBackend::Auto, None);
    let spec = template_catalog::default_entry().space;
    let Ok(files) =
        TemplateFiles::resolve(spec, env.shipped_root.as_deref(), env.user_root.as_deref())
    else {
        eprintln!("skipped: the shipped template is not found");
        return;
    };
    let status = describe(&files, &env);
    let Some(paths) = status.found else {
        eprintln!(
            "skipped: no flip transform of {spec} is cached (run `dbsloc group fliplr build`)"
        );
        return;
    };
    let (_, aff) = load_volume_f64(&files.t1).unwrap();
    let dims = {
        let (v, _) = load_volume_f64(&files.t1).unwrap();
        let d = v.dim();
        [d.0, d.1, d.2]
    };
    let centre = [12.0, -14.0, -6.0];
    let mut bytes = Vec::with_capacity(dims[0] * dims[1] * dims[2]);
    for k in 0..dims[2] {
        for j in 0..dims[1] {
            for i in 0..dims[0] {
                let w = fastwarp_core::apply_rowmajor(&aff, [i as f64, j as f64, k as f64]);
                let d2: f64 = (0..3).map(|a| (w[a] - centre[a]).powi(2)).sum();
                bytes.push(u8::from(d2 <= 16.0));
            }
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("sphere.nii");
    write_binary_nii(&src, dims, &bytes, &aff, Codes::default()).unwrap();
    let once = tmp.path().join("once.nii");
    let twice = tmp.path().join("twice.nii");
    flip_nifti(&src, &once, ImageKind::Binary, Some(&paths.forward)).unwrap();
    flip_nifti(&once, &twice, ImageKind::Binary, Some(&paths.forward)).unwrap();
    let a: Vec<f64> = bytes.iter().map(|&v| f64::from(v)).collect();
    let (vol, _) = load_volume_f64(&twice).unwrap();
    let mut b = Vec::with_capacity(a.len());
    for k in 0..dims[2] {
        for j in 0..dims[1] {
            for i in 0..dims[0] {
                b.push(vol[[i, j, k]]);
            }
        }
    }
    let d = dice(&a, &b);
    eprintln!("real template double flip Dice {d:.4}");
    assert!(d >= 0.9, "Dice {d}");

    let pts = [
        [12.0, -14.0, -6.0],
        [-11.0, -13.0, -8.0],
        [12.5, -10.0, 3.0],
    ];
    let mapped = flip_points_through(&pts, &paths.forward).unwrap();
    for (p, m) in pts.iter().zip(&mapped) {
        eprintln!("marker {p:?} -> {m:?}");
        assert!((p[0] + m[0]).abs() <= 2.0, "{p:?} -> {m:?}");
        assert!((p[1] - m[1]).abs() <= 2.0 && (p[2] - m[2]).abs() <= 2.0);
    }

    let mut grid = Vec::new();
    for x in -6..=6 {
        for y in -6..=6 {
            for z in -6..=6 {
                grid.push([f64::from(x) * 5.0, f64::from(y) * 5.0, f64::from(z) * 5.0]);
            }
        }
    }
    let through = flip_points_through(&grid, &paths.forward).unwrap();
    let plain = flip_points(&grid);
    let mut mags: Vec<f64> = through
        .iter()
        .zip(&plain)
        .map(|(a, b)| (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt())
        .collect();
    mags.sort_by(f64::total_cmp);
    let p99 = mags[(mags.len() - 1) * 99 / 100];
    eprintln!(
        "displacement vs plain flip over {} points: max {:.3} mm, p99 {p99:.3} mm",
        mags.len(),
        mags[mags.len() - 1]
    );
}
