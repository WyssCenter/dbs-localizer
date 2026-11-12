#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use fastwarp_gpu::parity::{check_mi, check_mi_chunked, Case};
use fastwarp_gpu::probe::{ProbeOptions, Probes, Tolerances};
use fastwarp_gpu::reference::Grid;
use fastwarp_gpu::{BackendSelect, BufferPool, GpuContext, GpuError, WgpuBackend};

fn require_adapter() -> bool {
    fastwarp_gpu::device::env_var("FASTWARP_GPU_REQUIRE_ADAPTER")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn context() -> Option<GpuContext> {
    match GpuContext::new(&BackendSelect::from_env().unwrap_or_default()) {
        Ok(c) => {
            eprintln!(
                "using adapter {} ({}, {})",
                c.info.name, c.info.backend, c.info.device_type
            );
            Some(c)
        }
        Err(GpuError::NoAdapter(why)) => {
            if require_adapter() {
                panic!("no GPU adapter with FASTWARP_GPU_REQUIRE_ADAPTER set: {why}");
            }
            eprintln!("skip: no adapter ({why})");
            None
        }
        Err(e) => panic!("device creation failed: {e}"),
    }
}

fn backend() -> Option<WgpuBackend> {
    match WgpuBackend::new(&BackendSelect::from_env().unwrap_or_default()) {
        Ok(b) => Some(b),
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

#[test]
fn mi_known_answer_matches_the_cpu_reference() {
    let Some(b) = backend() else { return };
    let mut results = check_mi(&b, &Case::selftest_mi()).unwrap();
    results.extend(check_mi_chunked(&b, &Case::selftest_mi()).unwrap());
    let failed: Vec<String> = results.iter().filter(|r| !r.ok).map(|r| r.line()).collect();
    for r in &results {
        eprintln!("[mi] {}", r.line());
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

fn options(nx: u32, ny: u32, nz: u32, radius: u32) -> ProbeOptions {
    ProbeOptions {
        grid: Grid { nx, ny, nz },
        repeat: 2,
        radius,
        axpy_iterations: 3,
        noop_dispatches: 64,
        noop_passes: 8,
        readbacks: 4,
        force_wall: false,
        pass_per_kernel: false,
        warmup_runs: 0,
    }
}

#[test]
fn axpy_matches_cpu() {
    let Some(ctx) = context() else { return };
    let mut p = Probes::new(&ctx, options(10, 10, 10, 2), Tolerances::default()).unwrap();
    let r = p.axpy().unwrap();
    assert!(r.correct, "axpy relative error {}", r.max_rel_err);
    let r = Probes::new(&ctx, options(37, 29, 13, 2), Tolerances::default())
        .unwrap()
        .axpy()
        .unwrap();
    assert!(r.correct, "axpy relative error {}", r.max_rel_err);
}

#[test]
fn cc_chain_matches_cpu_on_awkward_grids() {
    let Some(ctx) = context() else { return };
    for (nx, ny, nz, r) in [
        (19, 17, 13, 2),
        (45, 38, 34, 4),
        (300, 6, 5, 4),
        (6, 70, 5, 8),
        (9, 7, 75, 3),
    ] {
        let mut p = Probes::new(&ctx, options(nx, ny, nz, r), Tolerances::default()).unwrap();
        let res = p.cc_chain().unwrap();
        assert!(
            res.correct,
            "cc chain {nx}x{ny}x{nz} r={r}: derivative {:?}, value gpu {} cpu {} (rel {}), deterministic {}",
            res.derivative, res.value_gpu, res.value_cpu, res.value_rel_err, res.deterministic
        );
    }
}

#[test]
fn gather_matches_cpu() {
    let Some(ctx) = context() else { return };
    for (nx, ny, nz) in [(23, 19, 17), (40, 33, 21)] {
        let mut p = Probes::new(&ctx, options(nx, ny, nz, 2), Tolerances::default()).unwrap();
        let res = p.gather().unwrap();
        assert!(
            res.correct,
            "gather {nx}x{ny}x{nz}: abs err {}, validity mismatches {}, valid fraction {}, deterministic {}",
            res.max_abs_err, res.valid_mismatches, res.valid_fraction, res.deterministic
        );
    }
}

#[test]
fn dependent_dispatch_chain_counts_every_dispatch() {
    let Some(ctx) = context() else { return };
    let mut p = Probes::new(&ctx, options(8, 8, 8, 2), Tolerances::default()).unwrap();
    let res = p.dispatch_cost().unwrap();
    assert!(res.counter_ok);
    assert!(res.readback_us.median > 0.0);
}

#[test]
fn pool_reuses_released_buffers() {
    let Some(ctx) = context() else { return };
    let mut pool = BufferPool::new();
    let a = pool.acquire(&ctx, "a", 1000).unwrap();
    assert_eq!(a.size(), 1000);
    pool.release(a);
    let b = pool.acquire(&ctx, "b", 1000).unwrap();
    assert_eq!(pool.allocations(), 1);
    assert_eq!(pool.allocated_bytes(), 1000);
    drop(b);
}

#[test]
fn retain_sizes_drops_idle_buffers_that_do_not_match() {
    let Some(ctx) = context() else { return };
    const MIB: u64 = 1 << 20;
    let mut pool = BufferPool::new();
    let a = pool.acquire(&ctx, "a", 2 * MIB).unwrap();
    let b = pool.acquire(&ctx, "b", 3 * MIB).unwrap();
    let c = pool.acquire(&ctx, "c", 3 * MIB).unwrap();
    let small = pool.acquire(&ctx, "small", 4096).unwrap();
    pool.release(a);
    pool.release(b);
    pool.release(small);
    assert_eq!(pool.peak_bytes(), 8 * MIB + 4096);
    let freed = pool.retain_sizes(&[2 * MIB]);
    assert_eq!(freed, 3 * MIB);
    assert_eq!(pool.allocated_bytes(), 5 * MIB + 4096);
    assert_eq!(pool.idle_bytes(), 2 * MIB + 4096);
    assert_eq!(pool.peak_bytes(), 8 * MIB + 4096);
    drop(c);
    let again = pool.acquire(&ctx, "again", 2 * MIB).unwrap();
    assert_eq!(pool.allocations(), 4);
    drop(again);
}

#[test]
fn timed_reports_a_positive_duration() {
    let Some(ctx) = context() else { return };
    let t = ctx.timed(false, |_enc| {}).unwrap();
    assert!(t.wall_seconds > 0.0);
    assert!(t.seconds >= 0.0);
}
