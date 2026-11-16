// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fastwarp_cli::bench::{phantom, Size};
use fastwarp_reg::transform::{read_transform_file, TransformElem};
use fastwarp_reg::{Grid, Volume};
use serde_json::{json, Value};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_fastwarp")
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fastwarp_cli_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(args: &[&str]) -> Output {
    Command::new(exe()).args(args).output().unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// Whether the program finds a GPU; without one the GPU tests skip, unless
/// FASTWARP_GPU_REQUIRE_ADAPTER is set.
fn gpu() -> bool {
    let o = run(&["gpu-info"]);
    match code(&o) {
        0 => true,
        4 => {
            assert!(
                !truthy("FASTWARP_GPU_REQUIRE_ADAPTER"),
                "no GPU adapter with FASTWARP_GPU_REQUIRE_ADAPTER set: {}",
                text(&o.stderr)
            );
            eprintln!("skip: no GPU adapter ({})", text(&o.stderr).trim());
            false
        }
        c => panic!("gpu-info exit {c}: {}", text(&o.stderr)),
    }
}

fn rot_z(deg: f64) -> [[f64; 3]; 3] {
    let (s, c) = deg.to_radians().sin_cos();
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

fn oblique_volume() -> Volume<f32> {
    let grid = Grid::new(
        [12, 10, 8],
        [1.1, 0.9, 1.3],
        [-20.0, 15.0, -10.0],
        rot_z(12.0),
    )
    .unwrap();
    let data = (0..grid.num_voxels())
        .map(|i| ((i * 7919) % 1000) as f32 / 10.0)
        .collect();
    Volume::from_vec(grid, data).unwrap()
}

fn write_affine_txt(path: &Path, matrix: [f64; 9], translation: [f64; 3]) {
    let p: Vec<String> = matrix
        .iter()
        .chain(translation.iter())
        .map(|v| v.to_string())
        .collect();
    std::fs::write(
        path,
        format!(
            "#Insight Transform File V1.0\n#Transform 0\nTransform: AffineTransform_double_3_3\n\
             Parameters: {}\nFixedParameters: 0 0 0\n",
            p.join(" ")
        ),
    )
    .unwrap();
}

const IDENTITY: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

#[test]
fn help_lists_every_subcommand() {
    let o = run(&["--help"]);
    assert_eq!(code(&o), 0);
    let out = text(&o.stdout);
    for sub in [
        "register",
        "linear",
        "apply",
        "compose",
        "recipe",
        "gpu-info",
        "fits",
        "bench",
        "ants-registration",
        "ants-apply-transforms",
    ] {
        assert!(out.contains(sub), "{sub}");
    }
    assert!(out.contains("Usage: fastwarp <COMMAND>"), "{out}");
}

#[test]
fn ants_modes_pass_help_and_version_through() {
    for (sub, program) in [
        ("ants-registration", "antsRegistration"),
        ("ants-apply-transforms", "antsApplyTransforms"),
    ] {
        for flag in ["-h", "--help", "--version"] {
            let o = run(&[sub, flag]);
            assert_eq!(code(&o), 0, "{sub} {flag}: {}", text(&o.stderr));
            let out = text(&o.stdout);
            assert!(
                out.starts_with(&format!(
                    "fastwarp {} ({program} compatibility mode",
                    env!("CARGO_PKG_VERSION")
                )),
                "{sub} {flag}: {out}"
            );
            assert_eq!(out.contains("Supported:"), flag != "--version", "{out}");
        }
    }
    let o = run(&["ants-registration", "-d", "3", "--foo"]);
    assert_eq!(code(&o), 2);
    assert_eq!(
        text(&o.stderr).trim(),
        "fastwarp: error: ants-registration: unknown option --foo"
    );
}

#[test]
fn apply_through_the_identity_keeps_grid_and_values() {
    let d = tmp("identity");
    let vol = oblique_volume();
    let input = d.join("in.nii.gz");
    vol.save_nifti(&input).unwrap();
    let id = d.join("id.txt");
    write_affine_txt(&id, IDENTITY, [0.0; 3]);
    for interp in ["nearestNeighbor", "linear"] {
        let out = d.join(format!("out_{interp}.nii"));
        let o = run(&[
            "apply",
            "-r",
            s(&input),
            "-i",
            s(&input),
            "-o",
            s(&out),
            "-t",
            s(&id),
            "-n",
            interp,
        ]);
        assert_eq!(code(&o), 0, "{}", text(&o.stderr));
        assert!(text(&o.stdout).contains("wrote"));
        let back = Volume::<f32>::load_nifti(&out).unwrap();
        assert!(back.grid().approx_eq(vol.grid(), 1e-5), "{:?}", back.grid());
        let worst = back
            .data()
            .iter()
            .zip(vol.data())
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(worst < 1e-3, "{interp}: {worst}");
    }
}

#[test]
fn points_outside_take_a_negative_default_value() {
    let d = tmp("default_value");
    let vol = oblique_volume();
    let input = d.join("in.nii.gz");
    vol.save_nifti(&input).unwrap();
    let far = d.join("far.txt");
    write_affine_txt(&far, IDENTITY, [1000.0, 0.0, 0.0]);
    let out = d.join("out.nii");
    let o = run(&[
        "apply",
        "-r",
        s(&input),
        "-i",
        s(&input),
        "-o",
        s(&out),
        "-t",
        s(&far),
        "-f",
        "-5",
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    let back = Volume::<f32>::load_nifti(&out).unwrap();
    assert!(back.data().iter().all(|v| *v == -5.0));
}

#[test]
fn transforms_are_applied_in_ants_order() {
    let d = tmp("order");
    let grid = Grid::new([40, 4, 4], [1.0; 3], [0.0; 3], rot_z(0.0)).unwrap();
    let data = (0..grid.num_voxels())
        .map(|i| grid.ijk(i)[0] as f32)
        .collect();
    let ramp = d.join("ramp.nii");
    Volume::from_vec(grid, data)
        .unwrap()
        .save_nifti(&ramp)
        .unwrap();
    let ref_grid = Grid::new([8, 4, 4], [1.0; 3], [0.0; 3], rot_z(0.0)).unwrap();
    let reference = d.join("ref.nii");
    Volume::from_vec(ref_grid, vec![0f32; ref_grid.num_voxels()])
        .unwrap()
        .save_nifti(&reference)
        .unwrap();
    let shift = d.join("shift.txt");
    write_affine_txt(&shift, IDENTITY, [5.0, 0.0, 0.0]);
    let scale = d.join("scale.txt");
    write_affine_txt(
        &scale,
        [2.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        [0.0; 3],
    );
    let out = d.join("out.nii");
    let o = run(&[
        "apply",
        "-r",
        s(&reference),
        "-i",
        s(&ramp),
        "-o",
        s(&out),
        "-t",
        s(&shift),
        "-t",
        s(&scale),
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    let back = Volume::<f32>::load_nifti(&out).unwrap();
    let row: Vec<f32> = (0..5).map(|i| back.get(i, 1, 1).unwrap()).collect();
    assert_eq!(row, vec![10.0, 12.0, 14.0, 16.0, 18.0]);
}

#[test]
fn an_inverted_affine_is_applied_in_place_and_a_field_is_not_inverted() {
    let d = tmp("invert");
    let grid = Grid::new([40, 4, 4], [1.0; 3], [0.0; 3], rot_z(0.0)).unwrap();
    let data = (0..grid.num_voxels())
        .map(|i| grid.ijk(i)[0] as f32)
        .collect();
    let ramp = d.join("ramp.nii");
    Volume::from_vec(grid, data)
        .unwrap()
        .save_nifti(&ramp)
        .unwrap();
    let ref_grid = Grid::new([8, 4, 4], [1.0; 3], [0.0; 3], rot_z(0.0)).unwrap();
    let reference = d.join("ref.nii");
    Volume::from_vec(ref_grid, vec![0f32; ref_grid.num_voxels()])
        .unwrap()
        .save_nifti(&reference)
        .unwrap();
    let shift = d.join("shift.txt");
    write_affine_txt(&shift, IDENTITY, [-5.0, 0.0, 0.0]);
    let half = d.join("half.txt");
    write_affine_txt(
        &half,
        [0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        [0.0; 3],
    );
    let out = d.join("out.nii");
    let shift_inv = format!("[{},1]", s(&shift));
    let half_inv = format!("[ {} , 1 ]", s(&half));
    let o = run(&[
        "apply",
        "-r",
        s(&reference),
        "-i",
        s(&ramp),
        "-o",
        s(&out),
        "-t",
        &shift_inv,
        "-t",
        &half_inv,
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    let back = Volume::<f32>::load_nifti(&out).unwrap();
    let row: Vec<f32> = (0..5).map(|i| back.get(i, 1, 1).unwrap()).collect();
    assert_eq!(row, vec![10.0, 12.0, 14.0, 16.0, 18.0]);
    let plain = format!("[{},0]", s(&shift));
    let field = d.join("field.nii.gz");
    let o = run(&[
        "compose",
        "-r",
        s(&reference),
        "-o",
        s(&field),
        "-t",
        &plain,
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    let field_inv = format!("[{},1]", s(&field));
    let o = run(&[
        "apply",
        "-r",
        s(&reference),
        "-i",
        s(&ramp),
        "-o",
        s(&d.join("bad.nii")),
        "-t",
        &field_inv,
    ]);
    assert_eq!(code(&o), 2, "{}", text(&o.stderr));
    assert!(
        text(&o.stderr).contains("cannot invert the displacement field"),
        "{}",
        text(&o.stderr)
    );
    let o = run(&[
        "apply",
        "-r",
        s(&reference),
        "-i",
        s(&d.join("missing_input.nii")),
        "-o",
        s(&d.join("bad.nii")),
        "-t",
        &field_inv,
    ]);
    assert_eq!(code(&o), 2, "{}", text(&o.stderr));
    let nested = d.join("new").join("f.nii.gz");
    let o = run(&[
        "compose",
        "-r",
        s(&reference),
        "-o",
        s(&nested),
        "-t",
        &field_inv,
    ]);
    assert_eq!(code(&o), 2, "{}", text(&o.stderr));
    assert!(!d.join("new").exists());
    let o = run(&[
        "apply",
        "-r",
        s(&reference),
        "-i",
        s(&ramp),
        "-o",
        s(&d.join("bad.nii")),
        "-t",
        &format!("[{},2]", s(&shift)),
    ]);
    assert_eq!(code(&o), 2, "{}", text(&o.stderr));
    assert!(!d.join("bad.nii").exists());
}

#[test]
fn compose_writes_the_displacement_field() {
    let d = tmp("compose");
    let vol = oblique_volume();
    let reference = d.join("ref.nii.gz");
    vol.save_nifti(&reference).unwrap();
    let shift = d.join("shift.txt");
    write_affine_txt(&shift, IDENTITY, [1.5, -2.0, 0.25]);
    let field = d.join("sub/field.nii.gz");
    let o = run(&[
        "compose",
        "-r",
        s(&reference),
        "-o",
        s(&field),
        "-t",
        s(&shift),
        "--compression-level",
        "0",
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    let chain = read_transform_file(&field).unwrap();
    let TransformElem::DisplacementField(f) = &chain.queue()[0] else {
        panic!("not a field")
    };
    assert!(f.grid().approx_eq(vol.grid(), 1e-5));
    for v in f.data().chunks(3) {
        assert_eq!(v, [1.5, -2.0, 0.25]);
    }
}

#[test]
fn errors_have_distinct_exit_codes() {
    let d = tmp("errors");
    let vol = oblique_volume();
    let img = d.join("img.nii.gz");
    vol.save_nifti(&img).unwrap();
    let bad_tx = d.join("bad.txt");
    std::fs::write(&bad_tx, "not a transform").unwrap();
    let id_tx = d.join("id.txt");
    write_affine_txt(&id_tx, IDENTITY, [0.0; 3]);
    let cases: Vec<(Vec<&str>, i32, &str)> = vec![
        (
            vec!["register", "a.mha", "b.nii", "-o", "x_"],
            2,
            "only NIfTI",
        ),
        (
            vec![
                "apply",
                "-r",
                s(&img),
                "-i",
                "missing.nii",
                "-o",
                "o.nii",
                "-t",
                s(&id_tx),
            ],
            3,
            "no such file",
        ),
        (
            vec![
                "apply",
                "-r",
                s(&img),
                "-i",
                s(&img),
                "-o",
                "o.nii",
                "-t",
                s(&bad_tx),
            ],
            3,
            "reading",
        ),
        (
            vec![
                "apply",
                "-r",
                s(&img),
                "-i",
                s(&img),
                "-o",
                "o.nii",
                "-t",
                s(&bad_tx),
                "-n",
                "bspline",
            ],
            2,
            "not supported",
        ),
        (
            vec![
                "apply",
                "-r",
                s(&img),
                "-i",
                s(&img),
                "-o",
                "o.mha",
                "-t",
                s(&bad_tx),
            ],
            2,
            "output image",
        ),
        (vec!["fits", s(&img), "--iso-mm", "0"], 2, "iso_mm"),
        (
            vec!["bench", "--size", "tiny", "--recipe", "missing.json"],
            3,
            "recipe file",
        ),
    ];
    for (args, want, needle) in cases {
        let o = run(&args);
        let err = text(&o.stderr);
        assert_eq!(code(&o), want, "{args:?}: {err}");
        assert!(err.contains(needle), "{args:?}: {err}");
        assert!(err.starts_with("fastwarp: error: "), "{args:?}: {err}");
        assert_eq!(err.lines().count(), 1, "{args:?}: {err}");
        assert!(o.stdout.is_empty(), "{args:?}");
    }
    let bad_recipe = d.join("recipe.json");
    std::fs::write(&bad_recipe, r#"{"syn": {"iteration": [1]}}"#).unwrap();
    let o = run(&[
        "register",
        s(&img),
        s(&img),
        "-o",
        "x_",
        "--recipe",
        s(&bad_recipe),
    ]);
    assert_eq!(code(&o), 2);
    assert!(text(&o.stderr).contains("unknown key 'iteration'"));
}

#[test]
fn a_4d_image_is_refused() {
    let d = tmp("four_d");
    let path = d.join("vol4.nii");
    let header = nifti::NiftiHeader {
        dim: [4, 4, 4, 4, 2, 1, 1, 1],
        pixdim: [1.0; 8],
        datatype: 16,
        bitpix: 32,
        sform_code: 1,
        srow_x: [1.0, 0.0, 0.0, 0.0],
        srow_y: [0.0, 1.0, 0.0, 0.0],
        srow_z: [0.0, 0.0, 1.0, 0.0],
        ..Default::default()
    };
    let data = ndarray::Array4::<f32>::zeros((4, 4, 4, 2));
    nifti::writer::WriterOptions::new(&path)
        .reference_header(&header)
        .write_nifti(&data)
        .unwrap();
    let o = run(&["fits", s(&path)]);
    assert_eq!(code(&o), 2, "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("expected a 3-D image"));
}

#[test]
fn register_creates_no_output_folder_for_a_missing_input() {
    let d = tmp("no_folder");
    let input = d.join("in.nii.gz");
    oblique_volume().save_nifti(&input).unwrap();
    let missing = d.join("typo.nii.gz");
    let out = d.join("results").join("sub_");
    let o = run(&["register", s(&input), s(&missing), "-o", s(&out)]);
    assert_eq!(code(&o), 3, "{}", text(&o.stderr));
    assert!(!d.join("results").exists());
    let o = run(&["register", s(&missing), s(&input), "-o", s(&out)]);
    assert_eq!(code(&o), 3, "{}", text(&o.stderr));
    assert!(!d.join("results").exists());
}

fn small_recipe() -> Value {
    let linear = json!({"iterations": [40, 20], "shrink_factors": [2, 1],
                        "smoothing_sigmas": [1, 0]});
    json!({"rigid": linear, "affine": linear,
           "syn": {"iterations": [10, 5], "shrink_factors": [2, 1], "smoothing_sigmas": [1, 0]}})
}

fn ncc(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len() as f64;
    let ma = a.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
    let mb = b.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x) - ma, f64::from(*y) - mb);
        sab += x * y;
        saa += x * x;
        sbb += y * y;
    }
    sab / (saa * sbb).sqrt()
}

fn save_f32(v: &Volume<f64>, path: &Path) {
    let data = v.data().iter().map(|x| *x as f32).collect();
    Volume::from_vec(*v.grid(), data)
        .unwrap()
        .save_nifti(path)
        .unwrap();
}

struct Registered {
    dir: PathBuf,
    fixed: PathBuf,
    moving: PathBuf,
    prefix: PathBuf,
}

fn register_tiny(name: &str) -> Registered {
    let dir = tmp(name);
    let (fixed, moving) = phantom(Size::Tiny).unwrap();
    let fixed_path = dir.join("fixed.nii.gz");
    let moving_path = dir.join("moving.nii.gz");
    save_f32(&fixed, &fixed_path);
    save_f32(&moving, &moving_path);
    let recipe = dir.join("recipe.json");
    std::fs::write(&recipe, small_recipe().to_string()).unwrap();
    let prefix = dir.join("out").join("sub_");
    let o = run(&[
        "register",
        s(&fixed_path),
        s(&moving_path),
        "-o",
        s(&prefix),
        "--recipe",
        s(&recipe),
        "--json",
        "--write-warped",
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["grid"], json!([30, 36, 30]));
    assert_eq!(v["recipe"]["syn"]["iterations"], json!([10, 5]));
    assert_eq!(v["outputs"].as_array().unwrap().len(), 4);
    for f in v["outputs"].as_array().unwrap() {
        assert!(Path::new(f.as_str().unwrap()).is_file(), "{f}");
    }
    Registered {
        dir,
        fixed: fixed_path,
        moving: moving_path,
        prefix,
    }
}

#[test]
fn register_then_apply_aligns_the_phantom() {
    if !gpu() {
        return;
    }
    let r = register_tiny("register");
    let composite = format!("{}Composite.h5", s(&r.prefix));
    let warped = r.dir.join("warped.nii.gz");
    let o = run(&[
        "apply",
        "-r",
        s(&r.fixed),
        "-i",
        s(&r.moving),
        "-o",
        s(&warped),
        "-t",
        &composite,
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    let fixed = Volume::<f32>::load_nifti(&r.fixed).unwrap();
    let moving = Volume::<f32>::load_nifti(&r.moving).unwrap();
    let warped = Volume::<f32>::load_nifti(&warped).unwrap();
    let before = ncc(fixed.data(), moving.data());
    let after = ncc(fixed.data(), warped.data());
    assert!(after > before + 0.02, "NCC {before} -> {after}");

    let o = run(&["fits", s(&r.fixed), s(&r.moving), "--json"]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["fits"], json!(true));
    assert_eq!(v["grid"], json!([30, 36, 30]));
    assert_eq!(v["iso_mm"], json!(6.0));
    let o = run(&["fits", s(&r.fixed), "--iso-mm", "12", "--json"]);
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["grid"], json!([15, 18, 15]));
}

#[test]
fn linear_writes_an_affine_that_apply_reads() {
    if !gpu() {
        return;
    }
    let r = register_tiny("linear");
    let stage = r.dir.join("stage.json");
    std::fs::write(
        &stage,
        json!({"iterations": [40, 20], "shrink_factors": [2, 1], "smoothing_sigmas": [1, 0]})
            .to_string(),
    )
    .unwrap();
    let prefix = r.dir.join("lin").join("rigid_");
    let o = run(&[
        "linear",
        s(&r.fixed),
        s(&r.moving),
        "-o",
        s(&prefix),
        "--transform",
        "affine",
        "--stage",
        s(&stage),
        "--winsorize",
        "0.005,0.995",
        "--json",
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["transform"], "affine");
    assert_eq!(v["parameters"].as_array().unwrap().len(), 12);
    let mat = format!("{}0GenericAffine.mat", s(&prefix));
    assert!(Path::new(&mat).is_file());
    let again = r.dir.join("lin").join("again_");
    let o = run(&[
        "linear",
        s(&r.fixed),
        s(&r.moving),
        "-o",
        s(&again),
        "--initial",
        &mat,
        "--stage",
        s(&stage),
        "--json",
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    let w: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(w["transform"], "rigid");
    for k in 0..12 {
        let a = w["initial_parameters"][k].as_f64().unwrap();
        let b = v["parameters"][k].as_f64().unwrap();
        assert!((a - b).abs() < 1e-6, "initial parameter {k}: {a} vs {b}");
    }
    let o = run(&["linear", s(&r.fixed), s(&r.moving), "--stage", s(&stage)]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    assert!(text(&o.stdout).contains("rigid fit"));
    let o = run(&[
        "apply",
        "-r",
        s(&r.fixed),
        "-i",
        s(&r.moving),
        "-o",
        s(&r.dir.join("lin_warped.nii")),
        "-t",
        &mat,
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
}

#[test]
fn bench_runs_a_tiny_case() {
    if !gpu() {
        return;
    }
    let d = tmp("bench");
    let recipe = d.join("recipe.json");
    std::fs::write(&recipe, small_recipe().to_string()).unwrap();
    let o = run(&[
        "bench",
        "--size",
        "tiny",
        "--repeat",
        "2",
        "--recipe",
        s(&recipe),
        "--json",
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["warmup"], 1);
    assert_eq!(v["cases"][0]["name"], "tiny");
    assert_eq!(v["cases"][0]["status"], "ok");
    assert_eq!(v["cases"][0]["runs"].as_array().unwrap().len(), 2);
    assert!(v["machine"]["gpu"].as_str().is_some_and(|g| !g.is_empty()));
    assert!(v["cases"][0]["runs"][0]["wall_s"].as_f64().unwrap() > 0.0);
    let o = run(&[
        "bench",
        "--size",
        "tiny",
        "--recipe",
        s(&recipe),
        "--threads",
        "2",
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("2 threads (--threads)"), "{out}");
    assert!(out.contains("tiny        30x36x30"), "{out}");
}

fn ants_exe() -> Option<PathBuf> {
    let dir = std::env::var_os("FASTWARP_ANTS_DIR")?;
    let name = if cfg!(windows) {
        "antsApplyTransforms.exe"
    } else {
        "antsApplyTransforms"
    };
    let p = Path::new(&dir).join(name);
    assert!(
        p.is_file(),
        "FASTWARP_ANTS_DIR is set but {} is missing",
        p.display()
    );
    Some(p)
}

#[test]
#[ignore = "runs antsApplyTransforms from FASTWARP_ANTS_DIR and needs a GPU; run with --ignored"]
fn ants_apply_transforms_reads_the_registration() {
    let Some(ants) = ants_exe() else {
        eprintln!("skipped: set FASTWARP_ANTS_DIR to the directory holding antsApplyTransforms");
        return;
    };
    if !gpu() {
        return;
    }
    let r = register_tiny("ants");
    let composite = format!("{}Composite.h5", s(&r.prefix));
    let mine = r.dir.join("mine.nii.gz");
    let theirs = r.dir.join("theirs.nii.gz");
    let o = run(&[
        "apply",
        "-r",
        s(&r.fixed),
        "-i",
        s(&r.moving),
        "-o",
        s(&mine),
        "-t",
        &composite,
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o.stderr));
    let o = Command::new(&ants)
        .args([
            "-d",
            "3",
            "-r",
            s(&r.fixed),
            "-i",
            s(&r.moving),
            "-o",
            s(&theirs),
            "-t",
            &composite,
            "-n",
            "Linear",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    let a = Volume::<f32>::load_nifti(&mine).unwrap();
    let b = Volume::<f32>::load_nifti(&theirs).unwrap();
    assert!(a.grid().approx_eq(b.grid(), 1e-4));
    let worst = a
        .data()
        .iter()
        .zip(b.data())
        .map(|(x, y)| (x - y).abs())
        .fold(0f32, f32::max);
    assert!(
        worst < 1e-3,
        "max |fastwarp - antsApplyTransforms| = {worst}"
    );
}
