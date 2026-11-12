// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The WP1 probes: streaming bandwidth, CC chain, composite gather, dispatch cost, limits.

use std::time::Instant;

use serde::Serialize;

use crate::caps::{CapCheck, GpuCaps, Verdict};
use crate::device::{self, AdapterRecord, GpuContext};
use crate::error::{GpuError, GpuResult};
use crate::gates::{self, Extrapolation, GateRow, Stats};
use crate::kernels::{
    groups, AxpyParams, BoxParams, GridParams, Kernels, ReduceParams, WarpParams, BOX_RADIUS_MAX,
};
use crate::pool::BufferPool;
use crate::profiler::TimingMethod;
use crate::reference::{
    axpy_reference, cc_reference, fnv_f32, mean_f32, phantom_image, probe_affine, rim_mask,
    smooth_field, vector_error, warp_reference, ErrorStats, Grid, Lcg,
};

/// The 1.0 mm template grid of the plan (197 x 233 x 189, 8.68 M voxels).
pub const PROBE_GRID: Grid = Grid {
    nx: 197,
    ny: 233,
    nz: 189,
};

/// Probe configuration.
#[derive(Debug, Clone)]
pub struct ProbeOptions {
    pub grid: Grid,
    pub repeat: usize,
    pub radius: u32,
    pub axpy_iterations: u32,
    pub noop_dispatches: u32,
    pub noop_passes: u32,
    pub readbacks: u32,
    pub force_wall: bool,
    pub pass_per_kernel: bool,
    pub warmup_runs: usize,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        ProbeOptions {
            grid: PROBE_GRID,
            repeat: 7,
            radius: 4,
            axpy_iterations: 20,
            noop_dispatches: 10_000,
            noop_passes: 1_000,
            readbacks: 240,
            force_wall: false,
            pass_per_kernel: false,
            warmup_runs: 0,
        }
    }
}

/// Correctness tolerances (f32 GPU against f64 CPU).
#[derive(Debug, Clone, Copy)]
pub struct Tolerances {
    pub axpy_rel: f64,
    pub cc_derivative_rel_to_range: f64,
    pub cc_value_rel: f64,
    pub warp_abs: f64,
    pub warp_valid_mismatch_fraction: f64,
}

impl Default for Tolerances {
    fn default() -> Self {
        Tolerances {
            axpy_rel: 1e-5,
            cc_derivative_rel_to_range: 5e-3,
            cc_value_rel: 1e-4,
            warp_abs: 2e-4,
            warp_valid_mismatch_fraction: 2e-5,
        }
    }
}

/// Probe 1 result.
#[derive(Debug, Clone, Serialize)]
pub struct AxpyResult {
    pub voxels: usize,
    pub iterations_per_submission: u32,
    pub gb_s: Stats,
    pub ms_per_dispatch: Stats,
    pub wall_gb_s: Stats,
    pub method: TimingMethod,
    pub max_rel_err: f64,
    pub correct: bool,
}

/// Probe 2 result.
#[derive(Debug, Clone, Serialize)]
pub struct CcResult {
    pub grid: [u32; 3],
    pub radius: u32,
    pub ms: Stats,
    pub wall_ms: Stats,
    pub method: TimingMethod,
    pub stage_ms: Vec<(String, f64)>,
    pub derivative: ErrorStats,
    pub derivative_after_repeats: ErrorStats,
    pub value_rel_err_after_repeats: f64,
    pub value_gpu: f64,
    pub value_cpu: f64,
    pub value_rel_err: f64,
    pub deterministic: bool,
    pub correct: bool,
}

/// Probe 3 result.
#[derive(Debug, Clone, Serialize)]
pub struct GatherResult {
    pub grid: [u32; 3],
    pub ms: Stats,
    pub wall_ms: Stats,
    pub method: TimingMethod,
    pub gb_s_traffic_model: f64,
    pub max_abs_err: f64,
    pub valid_mismatches: usize,
    pub valid_fraction: f64,
    pub deterministic: bool,
    pub correct: bool,
}

/// Probe 4 result.
#[derive(Debug, Clone, Serialize)]
pub struct DispatchResult {
    pub dispatches: u32,
    pub passes: u32,
    pub dispatch_us: Stats,
    pub wall_dispatch_us: Stats,
    pub pass_us: Stats,
    pub wall_pass_us: Stats,
    pub readback_us: Stats,
    pub readback_p95_us: f64,
    pub method: TimingMethod,
    pub counter_ok: bool,
}

/// One surveyed adapter (probe 5).
#[derive(Debug, Clone, Serialize)]
pub struct AdapterCaps {
    pub info: AdapterRecord,
    pub caps: GpuCaps,
    pub checks: Vec<CapCheck>,
    pub verdict: Verdict,
    pub device_request: String,
    pub selected: bool,
}

/// Probe 5 result: all adapters per backend.
#[derive(Debug, Clone, Serialize)]
pub struct CapsSurvey {
    pub backends: Vec<(String, Vec<AdapterCaps>)>,
}

/// Which probes to run.
#[derive(Debug, Clone, Copy)]
pub struct ProbeSet {
    pub p1: bool,
    pub p2: bool,
    pub p3: bool,
    pub p4: bool,
    pub p5: bool,
}

impl ProbeSet {
    /// All five probes.
    pub fn all() -> Self {
        ProbeSet {
            p1: true,
            p2: true,
            p3: true,
            p4: true,
            p5: true,
        }
    }
}

/// Full report of one run.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeReport {
    pub adapter: AdapterRecord,
    pub caps: GpuCaps,
    pub grid: [u32; 3],
    pub repeat: usize,
    pub timing_method: TimingMethod,
    pub p1: Option<AxpyResult>,
    pub p2: Option<CcResult>,
    pub p3: Option<GatherResult>,
    pub p4: Option<DispatchResult>,
    pub p5: Option<CapsSurvey>,
    pub gates: Vec<GateRow>,
    pub extrapolation: Option<Extrapolation>,
    pub overall: Verdict,
    pub all_correct: bool,
}

fn compute_pass<'a>(enc: &'a mut wgpu::CommandEncoder) -> wgpu::ComputePass<'a> {
    enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default())
}

fn check_dispatch(ctx: &GpuContext, x: u32, y: u32, z: u32) -> GpuResult<()> {
    let max = ctx.adapter.limits().max_compute_workgroups_per_dimension;
    if x > max || y > max || z > max {
        return Err(GpuError::LimitsInsufficient {
            limit: "max_compute_workgroups_per_dimension",
            have: u64::from(max),
            need: u64::from(x.max(y).max(z)),
        });
    }
    Ok(())
}

/// Owns the context, kernels and buffer pool for a probe run.
pub struct Probes<'a> {
    pub ctx: &'a GpuContext,
    pub kernels: Kernels,
    pub pool: BufferPool,
    pub opt: ProbeOptions,
    pub tol: Tolerances,
}

impl<'a> Probes<'a> {
    /// Compiles the kernels.
    pub fn new(ctx: &'a GpuContext, opt: ProbeOptions, tol: Tolerances) -> GpuResult<Self> {
        if opt.radius > BOX_RADIUS_MAX {
            return Err(GpuError::Invalid(format!(
                "box radius {} exceeds {BOX_RADIUS_MAX}",
                opt.radius
            )));
        }
        Ok(Probes {
            ctx,
            kernels: Kernels::new(ctx)?,
            pool: BufferPool::new(),
            opt,
            tol,
        })
    }

    fn method(&self) -> TimingMethod {
        if self.ctx.has_timestamps() && !self.opt.force_wall {
            TimingMethod::GpuTimestamps
        } else {
            TimingMethod::WallClock
        }
    }

    fn acquire(&mut self, label: &str, bytes: u64) -> GpuResult<wgpu::Buffer> {
        self.pool.acquire(self.ctx, label, bytes)
    }

    /// Probe 1: `y = a*x + y`, repeated `axpy_iterations` times per submission.
    pub fn axpy(&mut self) -> GpuResult<AxpyResult> {
        let ctx = self.ctx;
        let n = self.opt.grid.n();
        let iters = self.opt.axpy_iterations.max(1);
        let a = 0.37f32;
        let mut rng = Lcg::new(11);
        let x: Vec<f32> = (0..n).map(|_| rng.next_f32()).collect();
        let y0: Vec<f32> = (0..n).map(|_| rng.next_f32()).collect();
        let xb = self.acquire("axpy x", n as u64 * 4)?;
        let yb = self.acquire("axpy y", n as u64 * 4)?;
        ctx.upload(&xb, &x);
        ctx.upload(&yb, &y0);
        let ub = ctx.uniform(
            "axpy params",
            &AxpyParams {
                n: n as u32,
                a,
                pad: [0; 2],
            },
        )?;
        let bg = ctx.bind(&self.kernels.axpy, &[&xb, &yb, &ub])?;
        let wgs = groups(n as u32, 256);
        check_dispatch(ctx, wgs, 1, 1)?;
        let force_wall = self.opt.force_wall;
        let pipeline = &self.kernels.axpy;
        let run = |count: u32| {
            ctx.timed(force_wall, |enc| {
                let mut p = compute_pass(enc);
                p.set_pipeline(pipeline);
                p.set_bind_group(0, &bg, &[]);
                for _ in 0..count {
                    p.dispatch_workgroups(wgs, 1, 1);
                }
            })
        };
        run(iters)?;
        let got = ctx.read_f32(&yb, n)?;
        let want = axpy_reference(&x, &y0, a, iters);
        let err = ErrorStats::compare(&got, &want, 1.0);
        let mut gb = Vec::new();
        let mut ms = Vec::new();
        let mut wall_gb = Vec::new();
        let bytes = 12.0 * n as f64;
        for _ in 0..self.opt.repeat.max(1) {
            let t = run(iters)?;
            let per = t.seconds / f64::from(iters);
            gb.push(bytes / per * 1e-9);
            ms.push(per * 1e3);
            wall_gb.push(bytes / (t.wall_seconds / f64::from(iters)) * 1e-9);
        }
        ctx.check_errors()?;
        self.pool.release(xb);
        self.pool.release(yb);
        Ok(AxpyResult {
            voxels: n,
            iterations_per_submission: iters,
            gb_s: Stats::from_samples(gb),
            ms_per_dispatch: Stats::from_samples(ms),
            wall_gb_s: Stats::from_samples(wall_gb),
            method: self.method(),
            max_rel_err: err.max_rel_to_range,
            correct: err.max_rel_to_range <= self.tol.axpy_rel,
        })
    }

    /// Probe 2: the CC chain (intermediates, three box passes, value and derivative, reduction).
    pub fn cc_chain(&mut self) -> GpuResult<CcResult> {
        let ctx = self.ctx;
        let g = self.opt.grid;
        let n = g.n();
        let r = self.opt.radius;
        let nb = n as u64 * 4;
        let img_i = phantom_image(g, 21, 0.35, 0.12);
        let base_j = phantom_image(g, 22, 0.35, 0.12);
        let img_j: Vec<f32> = img_i
            .iter()
            .zip(&base_j)
            .map(|(&a, &b)| 0.7 * a + 0.3 * b)
            .collect();
        let mask = rim_mask(g, 2);
        let (ki, kj) = (mean_f32(&img_i), mean_f32(&img_j));

        let b_i = self.acquire("cc img_i", nb)?;
        let b_j = self.acquire("cc img_j", nb)?;
        let b_m = self.acquire("cc mask", nb)?;
        let b_inter = self.acquire("cc inter", nb * 6)?;
        let b_a = self.acquire("cc box a", nb * 6)?;
        let b_b = self.acquire("cc box b", nb * 6)?;
        let b_ux = self.acquire("cc ux", nb)?;
        let b_uy = self.acquire("cc uy", nb)?;
        let b_uz = self.acquire("cc uz", nb)?;
        let wg = (groups(g.nx, 8), groups(g.ny, 8), groups(g.nz, 4));
        let n_partials = wg.0 * wg.1 * wg.2;
        let b_part = self.acquire("cc partials", u64::from(n_partials) * 4)?;
        let b_scalar = self.acquire("cc scalar", 16)?;
        ctx.upload(&b_i, &img_i);
        ctx.upload(&b_j, &img_j);
        ctx.upload(&b_m, &mask);

        let gp = GridParams {
            nx: g.nx,
            ny: g.ny,
            nz: g.nz,
            n: n as u32,
            shift_i: ki,
            shift_j: kj,
            pad: [0; 2],
        };
        let u_grid = ctx.uniform("cc grid", &gp)?;
        let box_params = |axis_len, axis_stride, other_stride| BoxParams {
            nx: g.nx,
            ny: g.ny,
            nz: g.nz,
            n: n as u32,
            axis_len,
            axis_stride,
            other_stride,
            radius: r,
            planes: 6,
            pad: [0; 3],
        };
        let u_bx = ctx.uniform("cc box x", &box_params(g.nx, 1, 0))?;
        let u_by = ctx.uniform("cc box y", &box_params(g.ny, g.nx, g.nx * g.ny))?;
        let u_bz = ctx.uniform("cc box z", &box_params(g.nz, g.nx * g.ny, g.nx))?;
        let u_red = ctx.uniform(
            "cc reduce",
            &ReduceParams {
                count: n_partials,
                pad: [0; 3],
            },
        )?;
        let k = &self.kernels;
        let bg_inter = ctx.bind(&k.cc_intermediates, &[&b_i, &b_j, &b_m, &b_inter, &u_grid])?;
        let bg_x = ctx.bind(&k.box_x, &[&b_inter, &b_a, &u_bx])?;
        let bg_y = ctx.bind(&k.box_yz, &[&b_a, &b_b, &u_by])?;
        let bg_z = ctx.bind(&k.box_yz, &[&b_b, &b_a, &u_bz])?;
        let bg_d = ctx.bind(
            &k.cc_derivative,
            &[&b_a, &b_inter, &b_ux, &b_uy, &b_uz, &b_part, &u_grid],
        )?;
        let bg_r = ctx.bind(&k.reduce_final, &[&b_part, &b_scalar, &u_red])?;
        let dims_x = (groups(g.nx, 256), g.ny, g.nz);
        let dims_y = (groups(g.nx, 32), groups(g.ny, 32), g.nz);
        let dims_z = (groups(g.nx, 32), groups(g.nz, 32), g.ny);
        for d in [wg, dims_x, dims_y, dims_z] {
            check_dispatch(ctx, d.0, d.1, d.2)?;
        }

        let stage_names = [
            "cc_intermediates",
            "box_x",
            "box_y",
            "box_z",
            "cc_value_derivative",
            "reduce_final",
        ];
        let encode = |p: &mut wgpu::ComputePass<'_>, stage: usize| match stage {
            0 => {
                p.set_pipeline(&k.cc_intermediates);
                p.set_bind_group(0, &bg_inter, &[]);
                p.dispatch_workgroups(wg.0, wg.1, wg.2);
            }
            1 => {
                p.set_pipeline(&k.box_x);
                p.set_bind_group(0, &bg_x, &[]);
                p.dispatch_workgroups(dims_x.0, dims_x.1, dims_x.2);
            }
            2 => {
                p.set_pipeline(&k.box_yz);
                p.set_bind_group(0, &bg_y, &[]);
                p.dispatch_workgroups(dims_y.0, dims_y.1, dims_y.2);
            }
            3 => {
                p.set_pipeline(&k.box_yz);
                p.set_bind_group(0, &bg_z, &[]);
                p.dispatch_workgroups(dims_z.0, dims_z.1, dims_z.2);
            }
            4 => {
                p.set_pipeline(&k.cc_derivative);
                p.set_bind_group(0, &bg_d, &[]);
                p.dispatch_workgroups(wg.0, wg.1, wg.2);
            }
            _ => {
                p.set_pipeline(&k.reduce_final);
                p.set_bind_group(0, &bg_r, &[]);
                p.dispatch_workgroups(1, 1, 1);
            }
        };
        let force_wall = self.opt.force_wall;
        let pass_per_kernel = self.opt.pass_per_kernel;
        let run_all = || {
            ctx.timed(force_wall, |enc| {
                if pass_per_kernel {
                    for s in 0..stage_names.len() {
                        let mut p = compute_pass(enc);
                        encode(&mut p, s);
                    }
                } else {
                    let mut p = compute_pass(enc);
                    for s in 0..stage_names.len() {
                        encode(&mut p, s);
                    }
                }
            })
        };

        for _ in 0..self.opt.warmup_runs {
            run_all()?;
        }
        run_all()?;
        let ux = ctx.read_f32(&b_ux, n)?;
        let uy = ctx.read_f32(&b_uy, n)?;
        let uz = ctx.read_f32(&b_uz, n)?;
        let value_gpu = f64::from(ctx.read_f32(&b_scalar, 1)?[0]);
        let first_hash = (
            fnv_f32(&ux),
            fnv_f32(&uy),
            fnv_f32(&uz),
            value_gpu.to_bits(),
        );

        let refr = cc_reference(g, &img_i, &img_j, &mask, ki, kj, r as usize);
        let derivative = vector_error([&ux, &uy, &uz], [&refr.ux, &refr.uy, &refr.uz]);
        let value_rel_err = (value_gpu - refr.value).abs() / refr.value.abs().max(1e-12);

        let mut ms = Vec::new();
        let mut wall = Vec::new();
        for _ in 0..self.opt.repeat.max(1) {
            let t = run_all()?;
            ms.push(t.seconds * 1e3);
            wall.push(t.wall_seconds * 1e3);
        }
        let ux2 = ctx.read_f32(&b_ux, n)?;
        let uy2 = ctx.read_f32(&b_uy, n)?;
        let uz2 = ctx.read_f32(&b_uz, n)?;
        let value2 = f64::from(ctx.read_f32(&b_scalar, 1)?[0]);
        let deterministic = first_hash
            == (
                fnv_f32(&ux2),
                fnv_f32(&uy2),
                fnv_f32(&uz2),
                value2.to_bits(),
            );
        let derivative_after_repeats =
            vector_error([&ux2, &uy2, &uz2], [&refr.ux, &refr.uy, &refr.uz]);
        let value_rel_err_after_repeats = (value2 - refr.value).abs() / refr.value.abs().max(1e-12);

        let mut stage_ms = Vec::new();
        for (s, name) in stage_names.iter().enumerate() {
            let mut v = Vec::new();
            for _ in 0..self.opt.repeat.max(1) {
                let t = ctx.timed(force_wall, |enc| {
                    let mut p = compute_pass(enc);
                    encode(&mut p, s);
                })?;
                v.push(t.seconds * 1e3);
            }
            stage_ms.push(((*name).to_string(), Stats::from_samples(v).median));
        }
        ctx.check_errors()?;
        for b in [
            b_i, b_j, b_m, b_inter, b_a, b_b, b_ux, b_uy, b_uz, b_part, b_scalar,
        ] {
            self.pool.release(b);
        }
        let correct = derivative.max_rel_to_range <= self.tol.cc_derivative_rel_to_range
            && value_rel_err <= self.tol.cc_value_rel
            && deterministic;
        Ok(CcResult {
            grid: [g.nx, g.ny, g.nz],
            radius: r,
            ms: Stats::from_samples(ms),
            wall_ms: Stats::from_samples(wall),
            method: self.method(),
            stage_ms,
            derivative,
            derivative_after_repeats,
            value_rel_err_after_repeats,
            value_gpu,
            value_cpu: refr.value,
            value_rel_err,
            deterministic,
            correct,
        })
    }

    /// Probe 3: trilinear gather of a displacement-field-plus-affine composite.
    pub fn gather(&mut self) -> GpuResult<GatherResult> {
        let ctx = self.ctx;
        let g = self.opt.grid;
        let n = g.n();
        let nb = n as u64 * 4;
        let src = phantom_image(g, 31, 0.4, 0.05);
        let fx = smooth_field(g, 32, 3.0);
        let fy = smooth_field(g, 33, 3.0);
        let fz = smooth_field(g, 34, 3.0);
        let affine = probe_affine(g);

        let b_src = self.acquire("warp src", nb)?;
        let b_fx = self.acquire("warp fx", nb)?;
        let b_fy = self.acquire("warp fy", nb)?;
        let b_fz = self.acquire("warp fz", nb)?;
        let b_out = self.acquire("warp out", nb)?;
        let b_valid = self.acquire("warp valid", nb)?;
        ctx.upload(&b_src, &src);
        ctx.upload(&b_fx, &fx);
        ctx.upload(&b_fy, &fy);
        ctx.upload(&b_fz, &fz);
        let wp = WarpParams {
            nx: g.nx,
            ny: g.ny,
            nz: g.nz,
            n: n as u32,
            snx: g.nx,
            sny: g.ny,
            snz: g.nz,
            flags: 1,
            vo: [0.0; 4],
            vs: [1.0; 4],
            so: [0.0; 4],
            ss: [1.0; 4],
            m0: affine[0],
            m1: affine[1],
            m2: affine[2],
        };
        let u = ctx.uniform("warp params", &wp)?;
        let bg = ctx.bind(
            &self.kernels.warp,
            &[&b_src, &b_fx, &b_fy, &b_fz, &b_out, &b_valid, &u],
        )?;
        let dims = (groups(g.nx, 8), groups(g.ny, 8), groups(g.nz, 4));
        check_dispatch(ctx, dims.0, dims.1, dims.2)?;
        let pipeline = &self.kernels.warp;
        let force_wall = self.opt.force_wall;
        let run = || {
            ctx.timed(force_wall, |enc| {
                let mut p = compute_pass(enc);
                p.set_pipeline(pipeline);
                p.set_bind_group(0, &bg, &[]);
                p.dispatch_workgroups(dims.0, dims.1, dims.2);
            })
        };
        run()?;
        let got = ctx.read_f32(&b_out, n)?;
        let got_valid = ctx.read_u32(&b_valid, n)?;
        let first_hash = (
            fnv_f32(&got),
            got_valid.iter().map(|&v| u64::from(v)).sum::<u64>(),
        );
        let (want, want_valid) = warp_reference(g, &src, [&fx, &fy, &fz], &affine);
        let mut mismatches = 0usize;
        let mut max_abs = 0.0f64;
        let mut valid_count = 0usize;
        for i in 0..n {
            let gv = got_valid[i] != 0;
            let wv = want_valid[i] != 0;
            if gv {
                valid_count += 1;
            }
            if gv != wv {
                mismatches += 1;
            } else if gv {
                max_abs = max_abs.max((f64::from(got[i]) - want[i]).abs());
            }
        }
        let mut ms = Vec::new();
        let mut wall = Vec::new();
        for _ in 0..self.opt.repeat.max(1) {
            let t = run()?;
            ms.push(t.seconds * 1e3);
            wall.push(t.wall_seconds * 1e3);
        }
        let got2 = ctx.read_f32(&b_out, n)?;
        let valid2 = ctx.read_u32(&b_valid, n)?;
        let deterministic = first_hash
            == (
                fnv_f32(&got2),
                valid2.iter().map(|&v| u64::from(v)).sum::<u64>(),
            );
        ctx.check_errors()?;
        for b in [b_src, b_fx, b_fy, b_fz, b_out, b_valid] {
            self.pool.release(b);
        }
        let stats = Stats::from_samples(ms);
        let allowed = (self.tol.warp_valid_mismatch_fraction * n as f64).max(8.0);
        let valid_fraction = valid_count as f64 / n as f64;
        let correct = max_abs <= self.tol.warp_abs
            && mismatches as f64 <= allowed
            && valid_fraction > 0.5
            && deterministic;
        Ok(GatherResult {
            grid: [g.nx, g.ny, g.nz],
            gb_s_traffic_model: 40.0 * n as f64 / (stats.median * 1e-3) * 1e-9,
            ms: stats,
            wall_ms: Stats::from_samples(wall),
            method: self.method(),
            max_abs_err: max_abs,
            valid_mismatches: mismatches,
            valid_fraction,
            deterministic,
            correct,
        })
    }

    /// Probe 4: dependent empty dispatches in one pass, one pass per dispatch, and
    /// submit + 8-byte readback latency.
    pub fn dispatch_cost(&mut self) -> GpuResult<DispatchResult> {
        let ctx = self.ctx;
        let counter = self.acquire("noop counter", 16)?;
        let bg = ctx.bind(&self.kernels.noop, &[&counter])?;
        let pipeline = &self.kernels.noop;
        let force_wall = self.opt.force_wall;
        let nd = self.opt.noop_dispatches.max(1);
        let np = self.opt.noop_passes.max(1);
        let one_pass = || {
            ctx.timed(force_wall, |enc| {
                let mut p = compute_pass(enc);
                p.set_pipeline(pipeline);
                p.set_bind_group(0, &bg, &[]);
                for _ in 0..nd {
                    p.dispatch_workgroups(1, 1, 1);
                }
            })
        };
        let many_passes = || {
            ctx.timed(force_wall, |enc| {
                for _ in 0..np {
                    let mut p = compute_pass(enc);
                    p.set_pipeline(pipeline);
                    p.set_bind_group(0, &bg, &[]);
                    p.dispatch_workgroups(1, 1, 1);
                }
            })
        };
        ctx.upload(&counter, &[0u32; 4]);
        one_pass()?;
        let after_one = ctx.read_u32(&counter, 1)?[0];
        many_passes()?;
        let after_both = ctx.read_u32(&counter, 1)?[0];
        let counter_ok = after_one == nd && after_both == nd + np;

        let mut d = Vec::new();
        let mut dw = Vec::new();
        let mut pa = Vec::new();
        let mut pw = Vec::new();
        for _ in 0..self.opt.repeat.max(1) {
            let t = one_pass()?;
            d.push(t.seconds / f64::from(nd) * 1e6);
            dw.push(t.wall_seconds / f64::from(nd) * 1e6);
            let t = many_passes()?;
            pa.push(t.seconds / f64::from(np) * 1e6);
            pw.push(t.wall_seconds / f64::from(np) * 1e6);
        }

        let staging = ctx.scoped("readback staging", 16, || {
            ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("noop staging"),
                size: 16,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        })?;
        let mut rb = Vec::new();
        for i in 0..self.opt.readbacks + 10 {
            let t0 = Instant::now();
            let mut enc = ctx
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            enc.copy_buffer_to_buffer(&counter, 0, &staging, 0, 8);
            ctx.queue.submit(Some(enc.finish()));
            let slice = staging.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            ctx.sync()?;
            match rx.recv() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(GpuError::Readback(e.to_string())),
                Err(e) => return Err(GpuError::Readback(e.to_string())),
            }
            let mapped = slice.get_mapped_range().map(|view| view.len());
            staging.unmap();
            mapped.map_err(|e| GpuError::Readback(e.to_string()))?;
            if i >= 10 {
                rb.push(t0.elapsed().as_secs_f64() * 1e6);
            }
        }
        ctx.check_errors()?;
        self.pool.release(counter);
        let readback = Stats::from_samples(rb);
        Ok(DispatchResult {
            dispatches: nd,
            passes: np,
            dispatch_us: Stats::from_samples(d),
            wall_dispatch_us: Stats::from_samples(dw),
            pass_us: Stats::from_samples(pa),
            wall_pass_us: Stats::from_samples(pw),
            readback_p95_us: readback.quantile(0.95),
            readback_us: readback,
            method: self.method(),
            counter_ok,
        })
    }
}

/// Probe 5: limits and features of every adapter on every backend, plus whether a device can be
/// created with the engine's required limits. `selected` marks the adapter of `ctx`.
pub fn survey_caps(ctx: Option<&GpuContext>) -> CapsSurvey {
    let mut backends = Vec::new();
    for (name, flag) in [
        ("Vulkan", wgpu::Backends::VULKAN),
        ("Dx12", wgpu::Backends::DX12),
        ("Metal", wgpu::Backends::METAL),
    ] {
        let instance = device::new_instance(flag);
        let adapters = device::enumerate(&instance, flag);
        let mut rows = Vec::new();
        for adapter in adapters {
            let info = AdapterRecord::from_info(&adapter.get_info());
            let mut caps = GpuCaps::from_adapter(&adapter);
            let device_request = match device::request_device(&adapter) {
                Ok((_d, q, _f)) => {
                    caps.timestamp_period_ns = Some(q.get_timestamp_period());
                    "ok".to_string()
                }
                Err(e) => e.to_string(),
            };
            let software = info.is_software();
            let checks = caps.requirements(software);
            let verdict = caps.verdict(&info.backend, software);
            let selected = ctx
                .map(|c| {
                    c.info.name == info.name
                        && c.info.backend == info.backend
                        && c.info.device == info.device
                })
                .unwrap_or(false);
            rows.push(AdapterCaps {
                info,
                caps,
                checks,
                verdict,
                device_request,
                selected,
            });
        }
        backends.push((name.to_string(), rows));
    }
    CapsSurvey { backends }
}

fn mib(b: u64) -> String {
    format!("{} MiB", b / (1024 * 1024))
}

/// Runs the selected probes and fills the gate table.
pub fn run_probes(
    ctx: &GpuContext,
    set: ProbeSet,
    opt: ProbeOptions,
    tol: Tolerances,
    progress: &mut dyn FnMut(&str),
) -> GpuResult<ProbeReport> {
    let grid = opt.grid;
    let repeat = opt.repeat;
    let mut probes = Probes::new(ctx, opt, tol)?;
    let timing_method = probes.method();
    let mut report = ProbeReport {
        adapter: ctx.info.clone(),
        caps: ctx.caps.clone(),
        grid: [grid.nx, grid.ny, grid.nz],
        repeat,
        timing_method,
        p1: None,
        p2: None,
        p3: None,
        p4: None,
        p5: None,
        gates: Vec::new(),
        extrapolation: None,
        overall: Verdict::Go,
        all_correct: true,
    };
    if set.p1 {
        progress("probe 1: streaming axpy");
        report.p1 = Some(probes.axpy()?);
    }
    if set.p2 {
        progress("probe 2: CC chain (includes CPU reference)");
        report.p2 = Some(probes.cc_chain()?);
    }
    if set.p3 {
        progress("probe 3: composite gather (includes CPU reference)");
        report.p3 = Some(probes.gather()?);
    }
    if set.p4 {
        progress("probe 4: dispatch and readback cost");
        report.p4 = Some(probes.dispatch_cost()?);
    }
    if set.p5 {
        progress("probe 5: limits and features");
        report.p5 = Some(survey_caps(Some(ctx)));
    }
    finalize(&mut report);
    Ok(report)
}

/// Fills `gates`, `extrapolation`, `overall` and `all_correct` from the probe results.
pub fn finalize(report: &mut ProbeReport) {
    let mut gates_out: Vec<GateRow> = Vec::new();
    let mut all_correct = true;
    if let Some(p) = &report.p1 {
        let v = if p.correct {
            gates::classify_bandwidth(p.gb_s.median)
        } else {
            Verdict::NoGo
        };
        all_correct &= p.correct;
        gates_out.push(GateRow {
            gate: "Probe 1 streaming y = a*x + y, 8.68 M f32".into(),
            measured: format!(
                "{:.1} GB/s (min {:.1}, max {:.1}; err {:.1e}{})",
                p.gb_s.median,
                p.gb_s.min,
                p.gb_s.max,
                p.max_rel_err,
                if p.correct { "" } else { " WRONG" }
            ),
            go: ">= 80 GB/s".into(),
            borderline: "50-80".into(),
            no_go: "< 50".into(),
            verdict: v,
        });
    }
    let p2_ms = report.p2.as_ref().map(|p| p.ms.median);
    if let Some(p) = &report.p2 {
        let v = if p.correct {
            gates::classify_cc(p.ms.median)
        } else {
            Verdict::NoGo
        };
        all_correct &= p.correct;
        gates_out.push(GateRow {
            gate: format!("Probe 2 CC r = {} chain", p.radius),
            measured: format!(
                "{:.2} ms (min {:.2}, max {:.2}; deriv err {:.1e}, value err {:.1e}{})",
                p.ms.median,
                p.ms.min,
                p.ms.max,
                p.derivative.max_rel_to_range,
                p.value_rel_err,
                if p.correct { "" } else { " WRONG" }
            ),
            go: "<= 70 ms".into(),
            borderline: "70-150".into(),
            no_go: "> 150".into(),
            verdict: v,
        });
    }
    let p3_ms = report.p3.as_ref().map(|p| p.ms.median);
    if let Some(p) = &report.p3 {
        let v = if p.correct {
            gates::classify_gather(p.ms.median)
        } else {
            Verdict::NoGo
        };
        all_correct &= p.correct;
        gates_out.push(GateRow {
            gate: "Probe 3 composite gather, 8.68 M".into(),
            measured: format!(
                "{:.2} ms (min {:.2}, max {:.2}; abs err {:.1e}, validity mismatches {}{})",
                p.ms.median,
                p.ms.min,
                p.ms.max,
                p.max_abs_err,
                p.valid_mismatches,
                if p.correct { "" } else { " WRONG" }
            ),
            go: "<= 15 ms".into(),
            borderline: "15-40".into(),
            no_go: "> 40".into(),
            verdict: v,
        });
    }
    if let Some(p) = &report.p4 {
        let v = if p.counter_ok {
            gates::classify_dispatch(p.dispatch_us.median, p.readback_us.median * 1e-3)
        } else {
            Verdict::NoGo
        };
        all_correct &= p.counter_ok;
        gates_out.push(GateRow {
            gate: "Probe 4 dispatch / readback".into(),
            measured: format!(
                "{:.2} us per dispatch (min {:.2}, max {:.2}); readback {:.3} ms (p95 {:.3})",
                p.dispatch_us.median,
                p.dispatch_us.min,
                p.dispatch_us.max,
                p.readback_us.median * 1e-3,
                p.readback_p95_us * 1e-3
            ),
            go: "<= 10 us, <= 1 ms".into(),
            borderline: "(unspecified)".into(),
            no_go: "> 50 us".into(),
            verdict: v,
        });
    }
    if report.p5.is_some() {
        let c = &report.caps;
        let v = c.verdict(&report.adapter.backend, report.adapter.is_software());
        gates_out.push(GateRow {
            gate: "Probe 5 limits (selected adapter)".into(),
            measured: format!(
                "binding {}, buffer {}, wg storage {} B, storage bufs {}, invocations {}, subgroups {}, timestamps {}",
                mib(c.max_storage_buffer_binding_size),
                mib(c.max_buffer_size),
                c.max_compute_workgroup_storage_size,
                c.max_storage_buffers_per_shader_stage,
                c.max_compute_invocations_per_workgroup,
                if c.subgroup { "yes" } else { "no" },
                if c.timestamp_query { "yes" } else { "no" },
            ),
            go: "all granted".into(),
            borderline: "DX12 without subgroups".into(),
            no_go: "binding <= 256 MiB".into(),
            verdict: v,
        });
    }
    if let (Some(a), Some(b)) = (p2_ms, p3_ms) {
        let e = gates::extrapolate_syn(a, b);
        gates_out.push(GateRow {
            gate: "Extrapolated 1.0 mm SyN".into(),
            measured: format!(
                "{:.1} s ({:.1} s with 0.3 s coarse overhead); N_eq {:.2}, {:.1} ms/iteration",
                e.total_s,
                e.total_s_with_overhead,
                e.finest_equivalent_iterations,
                e.per_iteration_ms
            ),
            go: "<= 30 s".into(),
            borderline: "<= 60 s".into(),
            no_go: "> 120 s (60-120 unspecified, treated as no-go)".into(),
            verdict: e.verdict,
        });
        report.extrapolation = Some(e);
    }
    let overall = gates_out
        .iter()
        .fold(Verdict::Go, |acc, r| acc.worst(r.verdict));
    report.gates = gates_out;
    report.overall = overall;
    report.all_correct = all_correct;
}

/// Renders the gate table and details as text.
pub fn render(report: &ProbeReport) -> String {
    let mut s = String::new();
    let a = &report.adapter;
    s.push_str(&format!(
        "adapter: {} | backend {} | type {} | driver '{}' '{}'\n",
        a.name, a.backend, a.device_type, a.driver, a.driver_info
    ));
    s.push_str(&format!(
        "grid {}x{}x{} | repeats {} | timing: {}\n",
        report.grid[0],
        report.grid[1],
        report.grid[2],
        report.repeat,
        report.timing_method.label()
    ));
    if let Some(p) = &report.caps.timestamp_period_ns {
        s.push_str(&format!("timestamp period: {p} ns\n"));
    }
    if let Some(p) = &report.p2 {
        s.push_str("probe 2 stage medians (ms): ");
        let parts: Vec<String> = p
            .stage_ms
            .iter()
            .map(|(n, v)| format!("{n} {v:.3}"))
            .collect();
        s.push_str(&parts.join(", "));
        s.push('\n');
        s.push_str(&format!(
            "probe 2 error after repeats: derivative {:.2e}, value {:.2e}\n",
            p.derivative_after_repeats.max_rel_to_range, p.value_rel_err_after_repeats
        ));
        s.push_str(&format!(
            "probe 2 value gpu {:.6} cpu {:.6}; deterministic {}; wall ms median {:.2} (min {:.2}, max {:.2})\n",
            p.value_gpu, p.value_cpu, p.deterministic, p.wall_ms.median, p.wall_ms.min, p.wall_ms.max
        ));
    }
    if let Some(p) = &report.p1 {
        s.push_str(&format!(
            "probe 1 wall-clock GB/s median {:.1}\n",
            p.wall_gb_s.median
        ));
    }
    if let Some(p) = &report.p3 {
        s.push_str(&format!(
            "probe 3 wall ms median {:.2}; traffic-model {:.1} GB/s at 40 B/voxel; valid fraction {:.3}; deterministic {}\n",
            p.wall_ms.median, p.gb_s_traffic_model, p.valid_fraction, p.deterministic
        ));
    }
    if let Some(p) = &report.p4 {
        s.push_str(&format!(
            "probe 4: per pass (1 dispatch/pass) {:.2} us gpu, {:.2} us wall; per dispatch wall {:.2} us; counter ok {}\n",
            p.pass_us.median, p.wall_pass_us.median, p.wall_dispatch_us.median, p.counter_ok
        ));
    }
    s.push_str("\nGate | Measured | Go | Borderline | No-go | Verdict\n");
    s.push_str("--- | --- | --- | --- | --- | ---\n");
    for r in &report.gates {
        s.push_str(&format!(
            "{} | {} | {} | {} | {} | {}\n",
            r.gate,
            r.measured,
            r.go,
            r.borderline,
            r.no_go,
            r.verdict.label()
        ));
    }
    if let Some(e) = &report.extrapolation {
        s.push_str(&format!("\n{}\n", e.formula));
        let lv: Vec<String> = e
            .levels
            .iter()
            .map(|(sh, it, f)| format!("{it} @ /{sh} (x{f:.6})"))
            .collect();
        s.push_str(&format!(
            "levels: {} -> N_eq = {:.4}; per iteration {:.2} ms; total {:.2} s (+0.3 s overhead = {:.2} s) -> {}\n",
            lv.join(", "),
            e.finest_equivalent_iterations,
            e.per_iteration_ms,
            e.total_s,
            e.total_s_with_overhead,
            e.verdict.label()
        ));
    }
    if let Some(sv) = &report.p5 {
        s.push_str("\nProbe 5 survey\n");
        for (name, rows) in &sv.backends {
            if rows.is_empty() {
                s.push_str(&format!("[{name}] no adapters\n"));
                continue;
            }
            for r in rows {
                s.push_str(&format!(
                    "[{name}] {}{} | type {} | driver '{}' '{}' | device request: {} | verdict {}\n",
                    r.info.name,
                    if r.selected { " (selected)" } else { "" },
                    r.info.device_type,
                    r.info.driver,
                    r.info.driver_info,
                    r.device_request,
                    r.verdict.label()
                ));
                let c = &r.caps;
                s.push_str(&format!(
                    "    max_storage_buffer_binding_size {} ({}), max_buffer_size {} ({}), max_uniform_buffer_binding_size {}\n    workgroup storage {} B, invocations {}, workgroup size {:?}, workgroups/dim {}, storage buffers/stage {}, bind groups {}, immediate size {}\n    timestamp period {:?} ns\n    features: {}\n",
                    c.max_storage_buffer_binding_size,
                    mib(c.max_storage_buffer_binding_size),
                    c.max_buffer_size,
                    mib(c.max_buffer_size),
                    c.max_uniform_buffer_binding_size,
                    c.max_compute_workgroup_storage_size,
                    c.max_compute_invocations_per_workgroup,
                    c.max_compute_workgroup_size,
                    c.max_compute_workgroups_per_dimension,
                    c.max_storage_buffers_per_shader_stage,
                    c.max_bind_groups,
                    c.max_immediate_size,
                    c.timestamp_period_ns,
                    c.feature_names.join(", "),
                ));
                for ck in &r.checks {
                    s.push_str(&format!(
                        "    [{}] {}: have {} (need {})\n",
                        if ck.ok { "ok" } else { "FAIL" },
                        ck.name,
                        ck.have,
                        ck.required
                    ));
                }
            }
        }
    }
    s.push_str(&format!(
        "\nOverall: {} | all correct: {}\n",
        report.overall.label(),
        report.all_correct
    ));
    s
}
