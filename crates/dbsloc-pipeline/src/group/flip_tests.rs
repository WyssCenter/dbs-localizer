#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use fastwarp_core::apply_rowmajor;

fn affine(ox: f64) -> Mat4 {
    [
        [0.5, 0.0, 0.0, ox],
        [0.0, 0.5, 0.0, -3.0],
        [0.0, 0.0, 0.5, 2.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

#[test]
fn flipping_negates_the_world_x_of_every_voxel() {
    let dims = [4, 2, 2];
    let aff = affine(10.0);
    let data: Vec<u8> = (0..16).collect();
    let (out, out_aff) = flip_x(&data, dims, &aff);
    for k in 0..2usize {
        for j in 0..2usize {
            for i in 0..4usize {
                let idx = i + 4 * (j + 2 * k);
                let before = apply_rowmajor(&aff, [i as f64, j as f64, k as f64]);
                let mut found = None;
                for i2 in 0..4usize {
                    let w = apply_rowmajor(&out_aff, [i2 as f64, j as f64, k as f64]);
                    if (w[0] + before[0]).abs() < 1e-12 {
                        found = Some(out[i2 + 4 * (j + 2 * k)]);
                        assert!((w[1] - before[1]).abs() < 1e-12);
                    }
                }
                assert_eq!(found, Some(data[idx]));
            }
        }
    }
    assert!(out_aff[0][0] > 0.0);
}

#[test]
fn flipping_twice_restores_data_and_affine() {
    let dims = [3, 2, 1];
    let aff = affine(1.5);
    let data = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
    let (once, a1) = flip_x(&data, dims, &aff);
    let (twice, a2) = flip_x(&once, dims, &a1);
    assert_eq!(twice, data);
    for r in 0..4 {
        for c in 0..4 {
            assert!((a2[r][c] - aff[r][c]).abs() < 1e-12);
        }
    }
}

#[test]
fn an_oblique_affine_is_mirrored_in_the_header_only() {
    let mut aff = affine(0.0);
    aff[0][1] = 0.1;
    let data = [1u8, 2, 3, 4];
    let (out, out_aff) = flip_x(&data, [2, 2, 1], &aff);
    assert_eq!(out, data);
    assert_eq!(out_aff[0][0], -0.5);
    assert_eq!(out_aff[0][1], -0.1);
}

#[test]
fn points_lose_the_sign_of_x_only() {
    assert_eq!(
        flip_points(&[[1.0, 2.0, 3.0], [-4.0, 0.0, 5.0]]),
        vec![[-1.0, 2.0, 3.0], [4.0, 0.0, 5.0]]
    );
}

#[test]
fn a_missing_transform_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.nii");
    let vol = ndarray::Array3::<u8>::zeros((4, 3, 2));
    crate::nifti_io::write_u8_3d(&src, &vol, &affine(5.0)).unwrap();
    let err = flip_nifti(
        &src,
        &dir.path().join("b.nii"),
        ImageKind::Binary,
        Some(Path::new("flip.h5")),
    )
    .unwrap_err();
    assert!(matches!(err, FlipError::Io(_)), "{err}");
    let err = flip_points_through(&[[1.0, 0.0, 0.0]], Path::new("flip.h5")).unwrap_err();
    assert!(matches!(err, FlipError::Unsupported(_)), "{err}");
}

#[test]
fn a_nifti_is_mirrored_and_a_second_flip_restores_it() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.nii");
    let mut vol = ndarray::Array3::<u8>::zeros((4, 3, 2));
    vol[[3, 1, 0]] = 1;
    vol[[2, 2, 1]] = 1;
    let aff = affine(5.0);
    crate::nifti_io::write_u8_3d(&src, &vol, &aff).unwrap();
    let once = dir.path().join("b.nii");
    flip_nifti(&src, &once, ImageKind::Binary, None).unwrap();
    let (flipped, flipped_aff) = crate::nifti_io::load_volume_f64(&once).unwrap();
    let mut worlds: Vec<[f64; 3]> = flipped
        .indexed_iter()
        .filter(|(_, v)| **v > 0.5)
        .map(|((i, j, k), _)| apply_rowmajor(&flipped_aff, [i as f64, j as f64, k as f64]))
        .collect();
    worlds.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let mut want: Vec<[f64; 3]> = [[3.0, 1.0, 0.0], [2.0, 2.0, 1.0]]
        .iter()
        .map(|p| {
            let w = apply_rowmajor(&aff, *p);
            [-w[0], w[1], w[2]]
        })
        .collect();
    want.sort_by(|a, b| a[0].total_cmp(&b[0]));
    assert_eq!(worlds.len(), 2);
    for (got, want) in worlds.iter().zip(&want) {
        for a in 0..3 {
            assert!((got[a] - want[a]).abs() < 1e-5, "{got:?} {want:?}");
        }
    }
    let twice = dir.path().join("c.nii");
    flip_nifti(&once, &twice, ImageKind::Binary, None).unwrap();
    let (back, _) = crate::nifti_io::load_volume_f64(&twice).unwrap();
    for ((i, j, k), v) in back.indexed_iter() {
        assert_eq!(*v, f64::from(vol[[i, j, k]]));
    }
}

#[test]
fn mirrored_names_follow_the_lead_dbs_pattern() {
    assert_eq!(
        flipped_name("01", "binary", "kuncel", Side::Right),
        "sub-01_sim-binary_model-kuncel_hemi-R_hemidesc-FlippedFromLeft.nii"
    );
    assert_eq!(
        flipped_name("01", "efield", "kuncel", Side::Left),
        "sub-01_sim-efield_model-kuncel_hemi-L_hemidesc-FlippedFromRight.nii"
    );
}

#[test]
fn a_label_folder_gets_the_opposite_side_flipped_and_stale_files_go() {
    let dir = tempfile::tempdir().unwrap();
    let mut vol = ndarray::Array3::<u8>::zeros((3, 3, 3));
    vol[[1, 1, 1]] = 1;
    crate::nifti_io::write_u8_3d(
        &dir.path()
            .join(original_name("01", "binary", "kuncel", Side::Right)),
        &vol,
        &affine(4.0),
    )
    .unwrap();
    let stale = dir
        .path()
        .join(flipped_name("01", "efield", "kuncel", Side::Right));
    std::fs::write(&stale, b"x").unwrap();
    let written = mirror_label_dir(dir.path(), "01", "kuncel", None).unwrap();
    assert_eq!(
        written,
        vec![dir
            .path()
            .join(flipped_name("01", "binary", "kuncel", Side::Left))]
    );
    assert!(!stale.exists());
    let removal = remove_flipped(dir.path());
    assert_eq!(removal.removed.len(), 1);
    assert!(removal.failed.is_empty());
}

#[test]
fn a_label_folder_that_cannot_be_listed_is_reported_and_a_missing_one_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let missing = remove_flipped(&dir.path().join("missing"));
    assert!(missing.removed.is_empty() && missing.failed.is_empty());
    let not_a_folder = dir.path().join("label");
    std::fs::write(&not_a_folder, b"x").unwrap();
    let removal = remove_flipped(&not_a_folder);
    assert!(removal.removed.is_empty());
    assert_eq!(removal.failed.len(), 1);
    assert_eq!(removal.failed[0].0, not_a_folder);
    assert!(removal.failed_list().contains("cannot list the folder"));
}

#[test]
fn a_flipped_file_that_cannot_be_deleted_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir
        .path()
        .join(flipped_name("01", "binary", "kuncel", Side::Right));
    std::fs::write(&gone, b"x").unwrap();
    let stuck = dir
        .path()
        .join(flipped_name("01", "efield", "kuncel", Side::Left));
    std::fs::create_dir(&stuck).unwrap();
    std::fs::write(stuck.join("held"), b"x").unwrap();
    let removal = remove_flipped(dir.path());
    assert_eq!(removal.removed, vec![gone.clone()]);
    assert_eq!(removal.failed.len(), 1);
    assert_eq!(removal.failed[0].0, stuck);
    assert!(removal.failed_list().contains(&stuck.display().to_string()));
    assert!(!gone.exists());

    let err = mirror_label_dir(dir.path(), "01", "kuncel", None).unwrap_err();
    assert!(
        err.to_string().contains(&stuck.display().to_string()),
        "{err}"
    );
}

#[test]
fn a_flipped_file_is_current_only_on_the_mirrored_grid_of_its_original() {
    let dir = tempfile::tempdir().unwrap();
    let dims = [4, 2, 2];
    let write = |name: &str, d: [usize; 3], a: &Mat4| {
        let p = dir.path().join(name);
        let n = d[0] * d[1] * d[2];
        write_binary_nii(&p, d, &vec![1; n], a, Codes::default()).unwrap();
        p
    };
    let original = write("o.nii", dims, &affine(10.0));
    let current = write("c.nii", dims, &mirrored_affine(dims, &affine(10.0)));
    let old = write("old.nii", dims, &affine(10.0));
    let other_dims = write("d.nii", [4, 2, 3], &mirrored_affine(dims, &affine(10.0)));
    let not_nifti = dir.path().join("x.nii");
    std::fs::write(&not_nifti, b"x").unwrap();
    assert!(flipped_is_current(&original, &current));
    assert!(!flipped_is_current(&original, &old));
    assert!(!flipped_is_current(&original, &other_dims));
    assert!(!flipped_is_current(&original, &not_nifti));
    assert!(!flipped_is_current(
        &original,
        &dir.path().join("missing.nii")
    ));

    let centred = write("s.nii", dims, &affine(-0.75));
    let same = write("s_flipped.nii", dims, &affine(-0.75));
    assert!(flipped_is_current(&centred, &same));
}
