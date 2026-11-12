#![allow(clippy::unwrap_used, clippy::needless_range_loop)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};

use fastwarp_reg::linalg::{vec3_norm, vec3_sub};
use fastwarp_reg::transform::itk_h5::{format_h5, read_h5_file};
use fastwarp_reg::transform::itk_mat::{format_mat, parse_mat, read_mat_file};
use fastwarp_reg::transform::itk_txt::read_text_file;
use fastwarp_reg::{map_points_ras, AffineTransform, TransformChain, TransformElem};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn only_affine(mut v: Vec<TransformElem>) -> AffineTransform {
    assert_eq!(v.len(), 1);
    match v.remove(0) {
        TransformElem::Affine(a) => a,
        TransformElem::DisplacementField(_) => panic!("expected an affine"),
    }
}

fn f64_at(bytes: &[u8], at: usize) -> f64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&bytes[at..at + 8]);
    f64::from_le_bytes(a)
}

#[test]
fn ants_mat_samples_match_an_independent_byte_decode() {
    for name in ["ms0.mat", "ms20.mat"] {
        let raw = std::fs::read(data(name)).unwrap();
        assert_eq!(raw.len(), 193);
        assert_eq!(&raw[20..46], b"AffineTransform_double_3_3");
        let a = only_affine(read_mat_file(&data(name)).unwrap());
        let p0 = 20 + 27;
        for i in 0..9 {
            assert_eq!(
                a.matrix[i / 3][i % 3],
                f64_at(&raw, p0 + 8 * i),
                "matrix {i}"
            );
        }
        for i in 0..3 {
            assert_eq!(
                a.translation[i],
                f64_at(&raw, p0 + 8 * (9 + i)),
                "translation {i}"
            );
        }
        let fixed_at = p0 + 96 + 20 + 6;
        assert_eq!(&raw[p0 + 96 + 20..fixed_at - 1], b"fixed");
        for i in 0..3 {
            assert_eq!(a.center[i], f64_at(&raw, fixed_at + 8 * i), "centre {i}");
        }
        let det = fastwarp_reg::linalg::mat3_det(&a.matrix);
        assert!(
            (det - 1.0).abs() < 1e-6,
            "ANTs rigid output has unit determinant, got {det}"
        );
        assert_eq!(format_mat(&[TransformElem::Affine(a)]).unwrap(), raw);
    }
}

#[test]
fn ants_mat_sample_has_the_expected_numbers() {
    let a = only_affine(read_mat_file(&data("ms0.mat")).unwrap());
    assert_eq!(a.matrix[0][0], 0.9998442385050702);
    assert_eq!(a.matrix[0][1], 0.01355877025201078);
    assert_eq!(a.matrix[2][2], 0.9998821761749128);
    assert_eq!(
        a.translation,
        [-2.675791891854692, -290.9203827342067, 148.534870757413]
    );
    assert_eq!(a.center, [-5.927373278, -3.158446675, -46.74841833]);
}

#[test]
fn text_samples_agree_with_their_rotation_definition() {
    let a0 = only_affine(read_text_file(&data("init0.txt")).unwrap());
    assert_eq!(
        a0.matrix,
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
    );
    assert_eq!(a0.translation, [-0.990540884, -293.8199077, 148.8418404]);
    assert_eq!(a0.center, [-5.927373278, -3.158446675, -46.74841833]);
    let a20 = only_affine(read_text_file(&data("init20.txt")).unwrap());
    let (s, c) = 20f64.to_radians().sin_cos();
    let want = [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]];
    for r in 0..3 {
        for k in 0..3 {
            assert!((a20.matrix[r][k] - want[r][k]).abs() < 1e-9, "[{r}][{k}]");
        }
    }
    assert_eq!(a20.center, a0.center);
    let p = [10.0, -20.0, 30.0];
    let expect = [
        p[0] - a20.center[0],
        c * (p[1] - a20.center[1]) - s * (p[2] - a20.center[2]),
        s * (p[1] - a20.center[1]) + c * (p[2] - a20.center[2]),
    ];
    let y = a20.apply(p);
    for i in 0..3 {
        assert!((y[i] - (expect[i] + a20.center[i] + a20.translation[i])).abs() < 1e-6);
    }
}

#[test]
fn text_and_mat_forms_of_the_same_transform_agree() {
    let a = only_affine(read_text_file(&data("init20.txt")).unwrap());
    let mat = format_mat(&[TransformElem::Affine(a)]).unwrap();
    let from_mat = only_affine(parse_mat(&mat).unwrap());
    assert_eq!(from_mat, a);
}

#[test]
fn ants_composite_sample_reads_and_rewrites_identically() {
    let chain = read_h5_file(&data("ctest.h5")).unwrap();
    assert_eq!(chain.len(), 2);
    let rewritten = fastwarp_reg::transform::itk_h5::parse_h5(format_h5(&chain).unwrap()).unwrap();
    assert_eq!(rewritten, chain);
}

fn parse_points(text: &str, key: &str) -> Vec<[f64; 3]> {
    let start = text.find(&format!("\"{key}\"")).unwrap();
    let rest = &text[start..];
    let open = rest.find("[[").unwrap();
    let close = rest.find("]]").unwrap();
    let body = &rest[open + 2..close];
    body.split("], [")
        .map(|row| {
            let v: Vec<f64> = row
                .trim_matches(|c| c == '[' || c == ']' || c == ' ')
                .split(',')
                .map(|t| t.trim().parse().unwrap())
                .collect();
            [v[0], v[1], v[2]]
        })
        .collect()
}

#[test]
fn prototype_composites_map_native_points_to_template_points() {
    let Some(dir) =
        fastwarp_core::env_var_os("FASTWARP_REG_PROTO_DIR").and_then(|v| v.into_string().ok())
    else {
        eprintln!("FASTWARP_REG_PROTO_DIR not set: skipping");
        return;
    };
    let dir = PathBuf::from(dir);
    let fwd = read_h5_file(&dir.join("fwd.h5")).unwrap();
    let inv = read_h5_file(&dir.join("inv.h5")).unwrap();
    assert_eq!(fwd.len(), 2);
    assert_eq!(inv.len(), 2);
    assert!(matches!(fwd.queue()[0], TransformElem::Affine(_)));
    assert!(matches!(
        fwd.queue()[1],
        TransformElem::DisplacementField(_)
    ));
    assert!(matches!(
        inv.queue()[0],
        TransformElem::DisplacementField(_)
    ));
    assert!(matches!(inv.queue()[1], TransformElem::Affine(_)));
    let text = std::fs::read_to_string(dir.join("points.json")).unwrap();
    let native = parse_points(&text, "native");
    let template = parse_points(&text, "template");
    assert_eq!(native.len(), template.len());
    let mapped = map_points_ras(&inv, &native);
    let reversed = TransformChain::from_queue(inv.queue().iter().rev().cloned().collect());
    let wrong = map_points_ras(&reversed, &native);
    let mut good = 0.0;
    let mut bad = 0.0;
    for ((m, w), t) in mapped.iter().zip(&wrong).zip(&template) {
        good += vec3_norm(vec3_sub(*m, *t));
        bad += vec3_norm(vec3_sub(*w, *t));
    }
    let n = native.len() as f64;
    eprintln!(
        "mean error InverseComposite order: {:.3} mm, reversed order: {:.3} mm",
        good / n,
        bad / n
    );
    assert!(good / n < 3.0, "mean error {} mm", good / n);
    assert!(good < bad, "the ITK order must beat the reversed order");
    for (n, t) in native.iter().zip(&template) {
        let there = map_points_ras(&fwd, &[*t])[0];
        let back = map_points_ras(&inv, &[there])[0];
        assert!(
            vec3_norm(vec3_sub(back, *t)) < 1.5,
            "forward/inverse round trip of {t:?} via {n:?}"
        );
    }
}

#[test]
fn real_ants_composite_parses_and_reports_its_queue_order() {
    let Some(path) =
        fastwarp_core::env_var_os("FASTWARP_REG_ANTS_COMPOSITE").and_then(|v| v.into_string().ok())
    else {
        eprintln!("FASTWARP_REG_ANTS_COMPOSITE not set: skipping");
        return;
    };
    let chain = read_h5_file(Path::new(&path)).unwrap();
    let kinds: Vec<&str> = chain
        .queue()
        .iter()
        .map(|e| match e {
            TransformElem::Affine(_) => "affine",
            TransformElem::DisplacementField(f) => {
                assert_eq!(f.data().len(), f.grid().num_voxels() * 3);
                "displacement field"
            }
        })
        .collect();
    eprintln!("ANTs composite queue order on disk: {kinds:?}");
    assert!(!kinds.is_empty());
}
