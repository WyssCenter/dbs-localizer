#![allow(clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use fastwarp_gpu::ab::{compare, initial_affine_from_composite, load_f64, render, SubjectFiles};
use fastwarp_gpu::{BackendSelect, WgpuBackend};
use fastwarp_reg::register::{
    register, register_full, write_linear_output, write_outputs, LinearPlan, RegisterInput,
};
use fastwarp_reg::syn::SynOptions;

#[test]
#[ignore = "needs a real subject folder in FASTWARP_REG_AB_DIR (ANTs outputs, 1 mm SyN) and runs for minutes; use --release"]
fn gpu_syn_agrees_with_ants_on_a_real_subject() {
    let Some(dir) = fastwarp_gpu::device::env_var_os("FASTWARP_REG_AB_DIR").map(PathBuf::from)
    else {
        eprintln!("FASTWARP_REG_AB_DIR not set; skipping");
        return;
    };
    let files = SubjectFiles::from_dir(&dir).expect("subject files");
    let gpu = match WgpuBackend::new(&BackendSelect::from_env().unwrap_or_default()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("no usable GPU adapter ({e}); skipping");
            return;
        }
    };
    let initial = initial_affine_from_composite(&files.ants_composite).expect("initial affine");
    let mut fixed = vec![load_f64(&files.template_t1).expect("template t1")];
    let mut moving = vec![load_f64(&files.moving_t1).expect("moving t1")];
    if let (Some(t2), Some(m2)) = (&files.template_t2, &files.moving_t2) {
        fixed.push(load_f64(t2).expect("template t2"));
        moving.push(load_f64(m2).expect("moving t2"));
    }
    let input = RegisterInput {
        fixed: &fixed,
        moving: &moving,
        initial,
        iso_mm: Some(1.0),
    };
    let out =
        register(&gpu, &files.recipe(), &input, &SynOptions::default()).expect("registration");
    drop((fixed, moving));
    let work = std::env::temp_dir().join(format!("fastwarp_reg_ab_{}", std::process::id()));
    std::fs::create_dir_all(&work).expect("work directory");
    let prefix = work.join("ours_");
    write_outputs(&prefix, &out).expect("write outputs");
    let report = compare(&files, &prefix, 1.0).expect("comparison");
    eprintln!("{}", render(&report));
    std::fs::remove_dir_all(&work).ok();
    assert!(
        report.ants_reproduction_pipeline_max_mm < 1e-3,
        "the reader no longer reproduces the stored template coordinates: {} mm",
        report.ants_reproduction_pipeline_max_mm
    );
    assert!(
        report.jacobian_forward_ours_mask.min > 0.0,
        "our forward warp folds inside the brain"
    );
    for p in &report.points_vs_ants {
        assert!(
            p.mean_mm < 2.0,
            "side {}: mean contact difference to ANTs {} mm",
            p.side,
            p.mean_mm
        );
    }
}

#[test]
#[ignore = "needs a real subject folder in FASTWARP_REG_AB_DIR and a hardware adapter; runs for about a minute on a GPU, hours on a software adapter; use --release"]
fn gpu_full_chain_without_ants_agrees_with_ants_on_a_real_subject() {
    let Some(dir) = fastwarp_gpu::device::env_var_os("FASTWARP_REG_AB_DIR").map(PathBuf::from)
    else {
        eprintln!("FASTWARP_REG_AB_DIR not set; skipping");
        return;
    };
    let files = SubjectFiles::from_dir(&dir).expect("subject files");
    let gpu = match WgpuBackend::new(&BackendSelect::from_env().unwrap_or_default()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("no usable GPU adapter ({e}); skipping");
            return;
        }
    };
    if gpu.context().info.is_software() {
        eprintln!("software adapter; skipping the real-data run");
        return;
    }
    let mut fixed = vec![load_f64(&files.template_t1).expect("template t1")];
    let mut moving = vec![load_f64(&files.moving_t1).expect("moving t1")];
    if let (Some(t2), Some(m2)) = (&files.template_t2, &files.moving_t2) {
        fixed.push(load_f64(t2).expect("template t2"));
        moving.push(load_f64(m2).expect("moving t2"));
    }
    let out = register_full(
        &gpu,
        &gpu,
        &LinearPlan::cohort(),
        &files.recipe(),
        &fixed,
        &moving,
        Some(1.0),
        &SynOptions::default(),
    )
    .expect("registration");
    drop((fixed, moving));
    let work = std::env::temp_dir().join(format!("fastwarp_reg_full_{}", std::process::id()));
    std::fs::create_dir_all(&work).expect("work directory");
    let prefix = work.join("ours_");
    write_outputs(&prefix, &out).expect("write outputs");
    if let Some(chain) = &out.linear {
        write_linear_output(&prefix, &chain.total).expect("write affine");
    }
    let report = compare(&files, &prefix, 1.0).expect("comparison");
    eprintln!("{}", render(&report));
    std::fs::remove_dir_all(&work).ok();
    assert!(
        report.affine_diff.max_mm < 1.0,
        "linear stages differ from ANTs by {} mm",
        report.affine_diff.max_mm
    );
    assert!(
        report.jacobian_forward_ours_mask.min > 0.0,
        "our forward warp folds inside the brain"
    );
    for p in &report.points_vs_ants {
        assert!(
            p.mean_mm < 0.5,
            "side {}: mean contact difference to ANTs {} mm",
            p.side,
            p.mean_mm
        );
    }
}
