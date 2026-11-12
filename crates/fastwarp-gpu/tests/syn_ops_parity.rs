#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use fastwarp_gpu::parity::{check_all, field, image, Case};
use fastwarp_gpu::{BackendSelect, GpuError, GpuField, WgpuBackend};
use fastwarp_reg::backend::Backend;
use fastwarp_reg::field_ops::InvertParams;
use fastwarp_reg::transform::AffineTransform;

fn require_adapter() -> bool {
    fastwarp_gpu::device::env_var("FASTWARP_GPU_REQUIRE_ADAPTER")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn backend() -> Option<WgpuBackend> {
    match WgpuBackend::new(&BackendSelect::from_env().unwrap_or_default()) {
        Ok(b) => {
            eprintln!("using {}", b.name());
            Some(b)
        }
        Err(GpuError::NoAdapter(why)) => {
            if require_adapter() {
                panic!("no GPU adapter with FASTWARP_GPU_REQUIRE_ADAPTER set: {why}");
            }
            eprintln!("skip: no adapter ({why})");
            None
        }
        Err(e) => panic!("backend creation failed: {e}"),
    }
}

fn run_case(b: &WgpuBackend, label: &str, case: &Case) {
    let results = check_all(b, case).unwrap();
    let mut failed = Vec::new();
    for r in &results {
        eprintln!("[{label}] {}", r.line());
        if !r.ok {
            failed.push(r.line());
        }
    }
    assert!(failed.is_empty(), "{label}: {}", failed.join("\n"));
}

#[test]
fn selftest_record_is_clean_on_creation() {
    let Some(b) = backend() else { return };
    let rec = b.selftest_record();
    eprintln!(
        "attempts {} first-attempt failures {:?}",
        rec.attempts, rec.first_attempt_failures
    );
    assert!(rec.attempts >= 1);
    assert!(!rec.lines.is_empty());
}

#[test]
fn ops_match_cpu_on_an_odd_oblique_anisotropic_grid() {
    let Some(b) = backend() else { return };
    run_case(
        &b,
        "oblique 19x15x13",
        &Case::awkward([19, 15, 13], [1.3, 2.1, 1.7], true, 3),
    );
}

#[test]
fn ops_match_cpu_on_an_axis_aligned_grid_with_long_axes() {
    let Some(b) = backend() else { return };
    run_case(
        &b,
        "aligned 41x9x7",
        &Case::awkward([41, 9, 7], [1.0, 1.5, 2.5], false, 4),
    );
    run_case(
        &b,
        "aligned 7x10x45",
        &Case::awkward([7, 10, 45], [2.0, 2.0, 1.0], false, 2),
    );
    run_case(
        &b,
        "aligned 300x5x4",
        &Case::awkward([300, 5, 4], [1.0, 2.0, 2.0], false, 6),
    );
}

#[test]
fn ops_match_cpu_on_a_larger_grid_crossing_workgroup_tiles() {
    let Some(b) = backend() else { return };
    run_case(
        &b,
        "oblique 53x37x29",
        &Case::awkward([53, 37, 29], [0.9, 1.1, 1.4], true, 4),
    );
}

#[test]
fn ops_match_cpu_on_a_degenerate_thin_grid() {
    let Some(b) = backend() else { return };
    run_case(
        &b,
        "thin 12x9x3",
        &Case::awkward([12, 9, 3], [1.0, 1.0, 3.0], false, 1),
    );
}

fn bits(v: &[f64]) -> Vec<u64> {
    v.iter().map(|x| x.to_bits()).collect()
}

#[test]
fn gpu_results_are_bit_identical_across_runs() {
    let Some(b) = backend() else { return };
    let case = Case::awkward([29, 23, 19], [1.2, 1.6, 2.0], true, 3);
    let grid = case.grid().unwrap();
    let fixed = b.upload_image(&image(&grid, 3, 0.2)).unwrap();
    let moving = b.upload_image(&image(&grid, 4, 0.2)).unwrap();
    let f0 = field(&grid, 2.0, 5);
    let run = || {
        let f = b.upload_field(&f0).unwrap();
        let w = b
            .warp_through_field(
                &moving,
                &f,
                Some(&AffineTransform::from_translation([0.4, -0.3, 0.2])),
            )
            .unwrap();
        let (value, d) = b.cc_value_derivative(&fixed, &w, 3).unwrap();
        let s = b.smooth_field(d, 3.0).unwrap();
        let m = b.max_scaled_norm(&s).unwrap();
        let l = b.l2_norm(&s).unwrap();
        let c = b.compose(&s, &f).unwrap();
        let (inv, stats) = b.invert(&c, f, &InvertParams::SYN).unwrap();
        let down = b.download_field(&inv).unwrap();
        let mut flat: Vec<f64> = down.data().iter().flat_map(|v| v.iter().copied()).collect();
        flat.extend([
            value,
            m,
            l,
            f64::from(stats.iterations),
            stats.mean_error_norm,
        ]);
        flat.extend(b.download_image(&w).unwrap());
        bits(&flat)
    };
    let a = run();
    let c = run();
    assert_eq!(a, c);
}

#[test]
fn an_inversion_longer_than_one_submission_runs_every_step_and_matches_the_cpu() {
    let Some(b) = backend() else { return };
    let cpu = fastwarp_reg::cpu::CpuBackend;
    let grid = Case::awkward([12, 10, 8], [1.0, 1.2, 1.5], false, 2)
        .grid()
        .unwrap();
    let f0 = field(&grid, 1.5, 7);
    let params = InvertParams {
        max_iterations: 1030,
        mean_error_tolerance: 0.0,
        max_error_tolerance: 0.0,
        enforce_boundary_condition: true,
    };
    let f = b.upload_field(&f0).unwrap();
    let (inverse, stats) = b.invert(&f, b.zero_field(&grid).unwrap(), &params).unwrap();
    let (cpu_inverse, cpu_stats) = cpu
        .invert(&f0, cpu.zero_field(&grid).unwrap(), &params)
        .unwrap();
    assert_eq!(stats.iterations, 1030);
    assert_eq!(cpu_stats.iterations, 1030);
    let gpu_inverse = b.download_field(&inverse).unwrap();
    let worst = gpu_inverse
        .data()
        .iter()
        .zip(cpu_inverse.data())
        .flat_map(|(g, c)| (0..3).map(move |k| (g[k] - c[k]).abs()))
        .fold(0.0f64, f64::max);
    assert!(worst < 1e-3, "{worst}");
}

#[test]
fn cc_of_an_image_with_itself_is_minus_one_with_zero_derivative() {
    let Some(b) = backend() else { return };
    let grid = Case::awkward([13, 11, 9], [1.0, 1.0, 1.0], false, 2)
        .grid()
        .unwrap();
    let img = b.upload_image(&image(&grid, 9, 0.3)).unwrap();
    let (value, d) = b.cc_value_derivative(&img, &img, 2).unwrap();
    assert!((value + 1.0).abs() < 1e-5, "{value}");
    let d = b.download_field(&d).unwrap();
    let worst = d
        .data()
        .iter()
        .flat_map(|v| v.iter())
        .fold(0.0f64, |a, x| a.max(x.abs()));
    assert!(worst < 1e-2, "{worst}");
}

#[test]
fn the_cc_pair_matches_the_cpu_in_both_directions_and_two_gpu_calls_bit_for_bit() {
    let Some(b) = backend() else { return };
    let cpu = fastwarp_reg::cpu::CpuBackend;
    for (dims, spacing, radius) in [
        ([29, 23, 19], [1.2, 1.6, 2.0], 3usize),
        ([13, 11, 9], [1.0, 1.0, 1.0], 2usize),
    ] {
        let grid = Case::awkward(dims, spacing, true, 3).grid().unwrap();
        let (fa, fb) = (image(&grid, 3, 0.2), image(&grid, 4, 0.2));
        let (ga, gb) = (b.upload_image(&fa).unwrap(), b.upload_image(&fb).unwrap());
        let ((v1, d1), (v2, d2)) = b.cc_value_derivative_pair(&ga, &gb, radius).unwrap();
        let (s1, e1) = b.cc_value_derivative(&ga, &gb, radius).unwrap();
        let (s2, e2) = b.cc_value_derivative(&gb, &ga, radius).unwrap();
        assert_eq!(v1.to_bits(), s1.to_bits());
        assert_eq!(v2.to_bits(), s2.to_bits());
        let down = |f| b.download_field(&f).unwrap();
        assert_eq!(down(d1).data(), down(e1).data());
        let swapped = down(d2);
        assert_eq!(swapped.data(), down(e2).data());
        let ((cv1, _), (cv2, cd2)) = cpu.cc_value_derivative_pair(&fa, &fb, radius).unwrap();
        assert!((v1 - cv1).abs() < 1e-4 * cv1.abs().max(1e-6), "{v1} {cv1}");
        assert!((v2 - cv2).abs() < 1e-4 * cv2.abs().max(1e-6), "{v2} {cv2}");
        let range = cd2
            .data()
            .iter()
            .flat_map(|v| v.iter())
            .fold(0.0f64, |a, x| a.max(x.abs()));
        let worst = swapped
            .data()
            .iter()
            .zip(cd2.data())
            .enumerate()
            .filter(|(i, _)| {
                let [x, y, z] = grid.ijk(*i);
                let [nx, ny, nz] = grid.size();
                x > 0 && y > 0 && z > 0 && x + 1 < nx && y + 1 < ny && z + 1 < nz
            })
            .flat_map(|(_, (g, c))| (0..3).map(move |k| (g[k] - c[k]).abs()))
            .fold(0.0f64, f64::max);
        assert!(
            worst < 1e-3 * range,
            "swapped derivative {worst} of {range}"
        );
    }
}

#[test]
fn paired_scalar_operations_match_the_single_ones_bit_for_bit_in_one_read_back() {
    let Some(b) = backend() else { return };
    let grid = Case::awkward([29, 23, 19], [1.2, 1.6, 2.0], true, 3)
        .grid()
        .unwrap();
    let fixed = b.upload_image(&image(&grid, 3, 0.2)).unwrap();
    let moving = b.upload_image(&image(&grid, 4, 0.2)).unwrap();
    let field_bits = |f: &GpuField| -> Vec<u64> {
        let down = b.download_field(f).unwrap();
        let flat: Vec<f64> = down.data().iter().flat_map(|v| v.iter().copied()).collect();
        bits(&flat)
    };
    let reads = || b.stats().scalar_readbacks;

    let before = reads();
    let (((v1, d1), (v2, d2)), [n1, n2]) = b
        .cc_value_derivative_pair_with_norms(&fixed, &moving, 3)
        .unwrap();
    assert_eq!(reads() - before, 1);
    let ((s1, e1), (s2, e2)) = b.cc_value_derivative_pair(&fixed, &moving, 3).unwrap();
    let (m1, m2) = (b.l2_norm(&e1).unwrap(), b.l2_norm(&e2).unwrap());
    assert_eq!(bits(&[v1, v2, n1, n2]), bits(&[s1, s2, m1, m2]));
    assert_eq!(field_bits(&d1), field_bits(&e1));
    assert_eq!(field_bits(&d2), field_bits(&e2));

    let fixed_side = b.smooth_field(d1, 3.0).unwrap();
    let moving_side = b.smooth_field(d2, 3.0).unwrap();
    let before = reads();
    let (max1, max2) = b.max_scaled_norm_pair(&fixed_side, &moving_side).unwrap();
    assert_eq!(reads() - before, 1);
    let single = [
        b.max_scaled_norm(&fixed_side).unwrap(),
        b.max_scaled_norm(&moving_side).unwrap(),
    ];
    assert_eq!(bits(&[max1, max2]), bits(&single));

    let f0 = field(&grid, 2.0, 5);
    let g0 = field(&grid, 0.5, 6);
    let (f, g) = (b.upload_field(&f0).unwrap(), b.upload_field(&g0).unwrap());
    let before = reads();
    let ((inverse, inverse_stats), (forward, forward_stats)) =
        b.invert_pair(f, g, &InvertParams::SYN).unwrap();
    assert_eq!(reads() - before, 1);
    let (f, g) = (b.upload_field(&f0).unwrap(), b.upload_field(&g0).unwrap());
    let (single_inverse, single_inverse_stats) = b.invert(&f, g, &InvertParams::SYN).unwrap();
    let (single_forward, single_forward_stats) =
        b.invert(&single_inverse, f, &InvertParams::SYN).unwrap();
    assert!(inverse_stats.iterations > 0 && forward_stats.iterations > 0);
    assert_eq!(inverse_stats, single_inverse_stats);
    assert_eq!(forward_stats, single_forward_stats);
    assert_eq!(field_bits(&inverse), field_bits(&single_inverse));
    assert_eq!(field_bits(&forward), field_bits(&single_forward));
}

#[test]
fn zero_field_is_zero_after_pool_reuse() {
    let Some(b) = backend() else { return };
    let grid = Case::awkward([10, 9, 8], [1.0, 1.0, 1.0], false, 2)
        .grid()
        .unwrap();
    {
        let f = b.upload_field(&field(&grid, 3.0, 1)).unwrap();
        drop(f);
    }
    let z = b.zero_field(&grid).unwrap();
    let down = b.download_field(&z).unwrap();
    assert!(down.data().iter().all(|v| v == &[0.0, 0.0, 0.0]));
}

#[test]
fn zero_field_trims_the_pool_once_per_grid_until_new_data_arrives() {
    let Some(b) = backend() else { return };
    let cube = |n: usize| {
        fastwarp_reg::Grid::new([n; 3], [1.0; 3], [0.0; 3], fastwarp_reg::linalg::IDENTITY3)
            .unwrap()
    };
    let (large, small) = (cube(64), cube(16));
    let large_field_bytes = 64 * 64 * 64 * 12;
    let small_field_bytes = 16 * 16 * 16 * 12;
    let large_field = b.zero_field(&large).unwrap();
    let _first = b.zero_field(&small).unwrap();
    drop(b.copy_field(&large_field).unwrap());
    let with_idle_copy = b.allocated_bytes();
    let _second = b.zero_field(&small).unwrap();
    assert_eq!(b.allocated_bytes(), with_idle_copy + small_field_bytes);
    let _image = b.upload_image(&image(&small, 1, 0.1)).unwrap();
    let after_upload = b.allocated_bytes();
    let _third = b.zero_field(&small).unwrap();
    assert_eq!(
        b.allocated_bytes(),
        after_upload - large_field_bytes + small_field_bytes
    );
    let resets: [(&str, &dyn Fn()); 3] = [
        ("resample_field", &|| {
            drop(b.resample_field(&large_field, &small).unwrap())
        }),
        ("upload_field", &|| {
            drop(b.upload_field(&field(&small, 1.0, 2)).unwrap())
        }),
        ("release_idle", &|| {
            b.release_idle();
        }),
    ];
    for (label, reset) in resets {
        let _trimmed = b.zero_field(&small).unwrap();
        reset();
        drop(b.copy_field(&large_field).unwrap());
        let with_idle_copy = b.allocated_bytes();
        let _after_reset = b.zero_field(&small).unwrap();
        assert!(
            b.allocated_bytes() + large_field_bytes <= with_idle_copy + small_field_bytes,
            "{label} did not make zero_field trim again"
        );
    }
}

#[test]
fn idle_pooled_buffers_are_released_on_request() {
    let Some(b) = backend() else { return };
    let grid = fastwarp_reg::Grid::new(
        [16, 16, 16],
        [1.0; 3],
        [0.0; 3],
        fastwarp_reg::linalg::IDENTITY3,
    )
    .unwrap();
    let field_bytes = 16 * 16 * 16 * 12;
    {
        let f = b.zero_field(&grid).unwrap();
        let _g = b.copy_field(&f).unwrap();
    }
    let before = b.allocated_bytes();
    let freed = b.release_idle();
    assert!(freed >= 2 * field_bytes, "{freed}");
    assert_eq!(b.allocated_bytes(), before - freed);
    let kept = b.zero_field(&grid).unwrap();
    assert_eq!(b.release_idle(), 0);
    drop(kept);
    assert_eq!(b.release_idle(), field_bytes);
    assert_eq!(b.allocated_bytes(), 0);
}
