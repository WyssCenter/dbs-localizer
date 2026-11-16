#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::*;
use fastwarp_gpu::{plan_capacity_linear, CapacityCaps};
use fastwarp_reg::apply::{apply_chain, Interpolation};
use fastwarp_reg::linear::LinearRecipe;
use fastwarp_reg::register::{register_full, LinearPlan};
use fastwarp_reg::stages::{register_stages, InitialTransform, StagePlan};
use fastwarp_reg::syn::SynOptions;
use fastwarp_reg::{AffineTransform, TransformChain, TransformElem, Volume};

fn shifted_head() -> (Volume<f64>, Volume<f64>) {
    let h = head([48; 3], 4.0, 3.0);
    let shift = TransformChain::from_queue(vec![TransformElem::Affine(
        AffineTransform::from_translation([6.0, -4.0, 3.0]),
    )]);
    let moving = apply_chain(&h.moving, &h.grid, &shift, Interpolation::Linear, 0.0).unwrap();
    (h.fixed, moving)
}

fn short(r: LinearRecipe) -> LinearRecipe {
    LinearRecipe {
        iterations: vec![20, 10, 5],
        shrink_factors: vec![4, 2, 1],
        smoothing_sigmas: vec![2.0, 1.0, 0.0],
        ..r
    }
}

fn mismatch(a: &Volume<f64>, b: &Volume<f64>) -> f64 {
    a.data()
        .iter()
        .zip(b.data())
        .map(|(x, y)| (x - y).powi(2))
        .sum()
}

#[test]
fn the_cohort_stage_plan_equals_register_full_bit_for_bit_on_the_gpu() {
    let (fixed, moving) = shifted_head();
    let linear = LinearPlan {
        rigid: short(LinearRecipe::cohort_rigid()),
        affine: short(LinearRecipe::cohort_affine()),
    };
    let syn = small_recipe(&[(2, 1.0, 4), (1, 0.0, 3)], 2);
    let options = SynOptions::default();
    let Some(a) = backend() else { return };
    let full = register_full(
        &a,
        &a,
        &linear,
        &syn,
        std::slice::from_ref(&fixed),
        std::slice::from_ref(&moving),
        None,
        &options,
    )
    .unwrap();
    drop(a);
    let Some(b) = backend() else { return };
    let staged = register_stages(
        &b,
        &b,
        &StagePlan::cohort(&linear, &syn),
        std::slice::from_ref(&fixed),
        std::slice::from_ref(&moving),
        None,
        &options,
    )
    .unwrap();
    assert_eq!(staged.total, full.linear.as_ref().unwrap().total);
    assert_eq!(staged.pair, full.pair);
    assert_eq!(staged.warped, full.warped);
}

#[test]
fn the_linear_capacity_plan_bounds_the_pool_peak_of_a_linear_only_chain() {
    let (fixed, moving) = shifted_head();
    let dense = |r: LinearRecipe| LinearRecipe {
        sampling_percentage: 1.0,
        ..short(r)
    };
    let stages = vec![
        dense(LinearRecipe::cohort_rigid()),
        short(LinearRecipe::cohort_affine()),
        dense(LinearRecipe::cohort_affine()),
    ];
    let plan = StagePlan {
        initial: InitialTransform::CentreOfMass,
        linear: stages.clone(),
        syn: None,
        winsorize: None,
    };
    let Some(gpu) = backend() else { return };
    let out = register_stages(
        &gpu,
        &gpu,
        &plan,
        std::slice::from_ref(&fixed),
        std::slice::from_ref(&moving),
        None,
        &SynOptions::default(),
    )
    .unwrap();
    assert!(mismatch(&out.warped, &fixed) < mismatch(&moving, &fixed));
    let peak = gpu.peak_allocated_bytes();
    let caps = CapacityCaps {
        max_storage_buffer_binding_size: u64::MAX,
        memory_budget: u64::MAX,
    };
    let planned = plan_capacity_linear([48; 3], [48; 3], &stages, &caps).unwrap();
    eprintln!(
        "linear chain: peak {peak} bytes, planned {} bytes",
        planned.level_total_bytes
    );
    assert!(peak > 0);
    assert!(
        peak <= planned.level_total_bytes,
        "peak {peak} > planned {}",
        planned.level_total_bytes
    );
    assert!(
        planned.level_total_bytes <= 3 * peak,
        "planned {} is far above the peak {peak}",
        planned.level_total_bytes
    );
}

#[test]
fn a_linear_then_syn_chain_peaks_no_higher_than_its_linear_plan_or_the_syn_stage_alone() {
    let (fixed, moving) = shifted_head();
    let dense = |r: LinearRecipe| LinearRecipe {
        sampling_percentage: 1.0,
        ..short(r)
    };
    let stages = vec![
        dense(LinearRecipe::cohort_rigid()),
        dense(LinearRecipe::cohort_affine()),
        dense(LinearRecipe::cohort_affine()),
    ];
    let plan = StagePlan {
        initial: InitialTransform::CentreOfMass,
        linear: stages.clone(),
        syn: Some(small_recipe(&[(2, 1.0, 3), (1, 0.0, 2)], 2)),
        winsorize: None,
    };
    let Some(gpu) = backend() else { return };
    register_stages(
        &gpu,
        &gpu,
        &plan,
        std::slice::from_ref(&fixed),
        std::slice::from_ref(&moving),
        None,
        &SynOptions::default(),
    )
    .unwrap();
    let peak = gpu.peak_allocated_bytes();
    let caps = CapacityCaps {
        max_storage_buffer_binding_size: u64::MAX,
        memory_budget: u64::MAX,
    };
    let linear = plan_capacity_linear([48; 3], [48; 3], &stages, &caps).unwrap();
    let syn = fastwarp_gpu::plan_capacity_syn(
        [48; 3],
        [48; 3],
        1,
        &plan.syn.as_ref().unwrap().shrink_factors,
        &caps,
    )
    .unwrap();
    eprintln!(
        "linear + SyN: peak {peak}, linear plan {}, SyN plan {}",
        linear.level_total_bytes, syn.level_total_bytes
    );
    let syn_only = StagePlan {
        initial: InitialTransform::CentreOfMass,
        linear: Vec::new(),
        ..plan.clone()
    };
    let Some(alone) = backend() else { return };
    register_stages(
        &alone,
        &alone,
        &syn_only,
        std::slice::from_ref(&fixed),
        std::slice::from_ref(&moving),
        None,
        &SynOptions::default(),
    )
    .unwrap();
    let syn_peak = alone.peak_allocated_bytes();
    eprintln!("SyN only: peak {syn_peak}");
    let bound = linear.level_total_bytes.max(syn_peak);
    assert!(peak <= bound, "peak {peak} > {bound}");
    let planned = linear.level_total_bytes.max(syn.level_total_bytes);
    assert!(peak <= planned, "peak {peak} > planned {planned}");
    assert!(
        syn_peak <= syn.level_total_bytes,
        "SyN peak {syn_peak} > planned {}",
        syn.level_total_bytes
    );
}

#[test]
fn a_syn_only_plan_from_the_identity_runs_on_the_gpu() {
    let h = head([40; 3], 4.0, 3.0);
    let plan = StagePlan {
        initial: InitialTransform::Identity,
        linear: Vec::new(),
        syn: Some(small_recipe(&[(2, 1.0, 6), (1, 0.0, 4)], 2)),
        winsorize: None,
    };
    let Some(gpu) = backend() else { return };
    let out = register_stages(
        &gpu,
        &gpu,
        &plan,
        std::slice::from_ref(&h.fixed),
        std::slice::from_ref(&h.moving),
        None,
        &SynOptions::default(),
    )
    .unwrap();
    assert!(out.deformable && out.linear.is_empty());
    assert_eq!(out.total, AffineTransform::identity());
    assert_eq!(out.pair.forward.len(), 2);
    assert!(mismatch(&out.warped, &h.fixed) < mismatch(&h.moving, &h.fixed));
}
