// SPDX-FileCopyrightText: 1999-2019 Insight Software Consortium
// SPDX-FileCopyrightText: 2020-present NumFOCUS
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0
//
// Copyright NumFOCUS (Insight Software Consortium)
// Copyright 2026 Wyss Center for Bio and Neuroengineering
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// Portions derived from ITK `itkN4BiasFieldCorrectionImageFilter.hxx` (the iteration loop),
// `itkBSplineScatteredDataPointSetToImageFilter.hxx` and `itkBSplineControlPointImageFilter.hxx`
// (the fit and the full-resolution reconstruction). See `docs/THIRD_PARTY_PORTS.md`.

//! In-process N4 on the GPU.
//!
//! The host keeps the `f64` control-point lattice and runs the histogram sharpening (512-point
//! FFT); the device runs the B-spline evaluation and the per-control-point gathered fit, the
//! `log(input) - log(bias)` step, the sharpening table lookup and `input / exp(field)`.
//! The deviations from the ITK / ANTs algorithm are the `f32` kernels and the gather order (both shared with the
//! CPU reference, which is the parity oracle).

use bytemuck::Pod;
use fastwarp_reg::n4::{
    apply_sharpen_lut, convergence_measurement, eval_lattice, refine_lattice, sharpen_lut,
    ControlLattice, FitPlan, N4LevelTrace, N4Outcome, N4Params, SharpenLut, N4_SPLINE_ORDER,
};
use fastwarp_reg::pyramid::shrink_volume;
use fastwarp_reg::volume::Volume;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::device::{BackendSelect, GpuContext};
use crate::error::{GpuError, GpuResult};
use crate::n4_kernels::{DivU, EvalU, FitU, LogU, LutU, N4Kernels};

const WG: u32 = 64;
const FIT_TARGET_INVOCATIONS: u32 = 4096;
const FIT_MAX_SLABS: u32 = 64;
const STORAGE: wgpu::BufferUsages = wgpu::BufferUsages::STORAGE
    .union(wgpu::BufferUsages::COPY_SRC)
    .union(wgpu::BufferUsages::COPY_DST);
const SELFTEST_ATTEMPTS: u32 = 3;

fn reg(e: fastwarp_reg::RegError) -> GpuError {
    GpuError::Invalid(e.to_string())
}

/// Record of the N4 device-creation known-answer check, one entry per kernel family.
#[derive(Debug, Clone, Default)]
pub struct N4SelfTestRecord {
    /// Attempts the worst family needed (more than 1 means a family was recompiled).
    pub attempts: u32,
    /// Attempts `k_n4_eval_lattice` needed.
    pub eval_attempts: u32,
    /// Attempts `k_n4_fit_gather` needed.
    pub fit_attempts: u32,
    /// Attempts `k_n4_log_residual_hist` needed.
    pub log_attempts: u32,
    /// Attempts `k_n4_apply_lut` needed.
    pub lut_attempts: u32,
    /// Attempts `k_n4_divide_exp` needed.
    pub div_attempts: u32,
    /// Result lines of the passing attempt.
    pub lines: Vec<String>,
    /// Mismatch lines of the failed first attempt, when a retry was needed.
    pub first_attempt_failures: Vec<String>,
}

#[derive(Clone, Copy)]
enum N4Family {
    Eval,
    Fit,
    Log,
    Lut,
    Div,
}

impl N4Family {
    fn label(self) -> &'static str {
        match self {
            N4Family::Eval => "eval_lattice",
            N4Family::Fit => "fit_gather",
            N4Family::Log => "log_residual",
            N4Family::Lut => "apply_lut",
            N4Family::Div => "divide_exp",
        }
    }

    const ALL: [N4Family; 5] = [
        N4Family::Eval,
        N4Family::Fit,
        N4Family::Log,
        N4Family::Lut,
        N4Family::Div,
    ];
}

struct Grown {
    buffer: wgpu::Buffer,
    capacity: u64,
}

/// N4 on the device, with its own known-answer check.
pub struct GpuN4 {
    ctx: Arc<GpuContext>,
    kernels: N4Kernels,
    max_groups: u32,
    selftest: N4SelfTestRecord,
}

impl GpuN4 {
    /// Selects an adapter, compiles the kernels and runs the known-answer check.
    pub fn new(select: &BackendSelect) -> GpuResult<Self> {
        GpuContext::new_with(select, Self::from_context)
    }

    /// Wraps an existing context and runs the known-answer check. A mismatch on a family
    /// recompiles only that family and retries, up to `SELFTEST_ATTEMPTS` attempts; a mismatch that persists fails.
    pub fn from_context(ctx: GpuContext) -> GpuResult<Self> {
        Self::from_shared_context(Arc::new(ctx))
    }

    /// Like [`Self::from_context`] on a context shared with other engines (`WgpuBackend`).
    pub fn from_shared_context(ctx: Arc<GpuContext>) -> GpuResult<Self> {
        let max_groups = ctx.device.limits().max_compute_workgroups_per_dimension;
        let mut gpu = GpuN4 {
            kernels: N4Kernels::new(&ctx)?,
            ctx,
            max_groups,
            selftest: N4SelfTestRecord::default(),
        };
        gpu.selftest()?;
        Ok(gpu)
    }

    /// The record of the device-creation known-answer check.
    pub fn selftest_record(&self) -> &N4SelfTestRecord {
        &self.selftest
    }

    /// The device context.
    pub fn context(&self) -> &GpuContext {
        &self.ctx
    }

    /// A shared handle to the device context.
    pub fn shared_context(&self) -> Arc<GpuContext> {
        Arc::clone(&self.ctx)
    }

    fn storage(&self, label: &str, bytes: u64) -> GpuResult<wgpu::Buffer> {
        self.ctx.scoped(label, bytes.max(4), || {
            self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.max(4),
                usage: STORAGE,
                mapped_at_creation: false,
            })
        })
    }

    fn upload<T: Pod>(&self, buf: &wgpu::Buffer, data: &[T]) {
        self.ctx
            .queue
            .write_buffer(buf, 0, bytemuck::cast_slice(data));
    }

    fn ensure(
        &self,
        slot: &mut Option<Grown>,
        label: &str,
        bytes: u64,
        staging: bool,
    ) -> GpuResult<()> {
        let need = bytes.max(4);
        if slot.as_ref().is_some_and(|g| g.capacity >= need) {
            return Ok(());
        }
        let buffer = if staging {
            self.ctx.staging_buffer(need)?
        } else {
            self.storage(label, need)?
        };
        *slot = Some(Grown {
            buffer,
            capacity: need,
        });
        Ok(())
    }

    fn slot(slot: &Option<Grown>) -> GpuResult<&wgpu::Buffer> {
        slot.as_ref()
            .map(|g| &g.buffer)
            .ok_or_else(|| GpuError::Internal("n4 scratch buffer missing".into()))
    }

    fn flat_groups(&self, items: u32) -> ([u32; 3], u32) {
        flat_groups_with(items, self.max_groups)
    }

    fn run(
        &self,
        label: &'static str,
        pipeline: &wgpu::ComputePipeline,
        entries: &[(u32, &wgpu::Buffer)],
        groups: [u32; 3],
    ) -> GpuResult<()> {
        let layout = pipeline.get_bind_group_layout(0);
        let bindings: Vec<wgpu::BindGroupEntry> = entries
            .iter()
            .map(|(b, buf)| wgpu::BindGroupEntry {
                binding: *b,
                resource: buf.as_entire_binding(),
            })
            .collect();
        let group = self.ctx.scoped(label, 0, || {
            self.ctx
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: &layout,
                    entries: &bindings,
                })
        })?;
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(label),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(groups[0], groups[1], groups[2]);
        }
        self.ctx.queue.submit(Some(enc.finish()));
        self.ctx.check_errors()
    }

    fn eval(
        &self,
        lattice: &ControlLattice,
        out_size: [usize; 3],
        out_buf: &wgpu::Buffer,
        lat_buf: &wgpu::Buffer,
    ) -> GpuResult<()> {
        let (un, groups) = self.eval_args(lattice.size, out_size);
        let uniform = self.ctx.uniform("n4 eval", &un)?;
        self.run_eval(&uniform, groups, out_buf, lat_buf)
    }

    fn run_eval(
        &self,
        uniform: &wgpu::Buffer,
        groups: [u32; 3],
        out_buf: &wgpu::Buffer,
        lat_buf: &wgpu::Buffer,
    ) -> GpuResult<()> {
        self.run(
            "n4 eval",
            &self.kernels.eval_lattice,
            &[(0, lat_buf), (1, out_buf), (2, uniform)],
            groups,
        )
    }

    fn eval_args(&self, lattice_size: [usize; 3], out_size: [usize; 3]) -> (EvalU, [u32; 3]) {
        let order = N4_SPLINE_ORDER;
        let n = (out_size[0] * out_size[1] * out_size[2]) as u32;
        let (groups, stride) = self.flat_groups(n);
        let un = EvalU {
            lat: [
                lattice_size[0] as u32,
                lattice_size[1] as u32,
                lattice_size[2] as u32,
                order as u32,
            ],
            outd: [
                out_size[0] as u32,
                out_size[1] as u32,
                out_size[2] as u32,
                n,
            ],
            scale: [
                (lattice_size[0] - order) as f32 / (out_size[0] - 1) as f32,
                (lattice_size[1] - order) as f32 / (out_size[1] - 1) as f32,
                (lattice_size[2] - order) as f32 / (out_size[2] - 1) as f32,
                0.5 * (order as f32 - 1.0),
            ],
            disp: [stride, 0, 0, 0],
        };
        (un, groups)
    }

    fn fit(
        &self,
        plan: &FitPlan,
        residual_buf: &wgpu::Buffer,
        bounds_buf: &wgpu::Buffer,
        w2_buf: &wgpu::Buffer,
        numden_buf: &wgpu::Buffer,
    ) -> GpuResult<()> {
        let (un, groups) = self.fit_args(plan);
        let uniform = self.ctx.uniform("n4 fit", &un)?;
        self.run_fit(
            &uniform,
            groups,
            residual_buf,
            bounds_buf,
            w2_buf,
            numden_buf,
        )
    }

    fn run_fit(
        &self,
        uniform: &wgpu::Buffer,
        groups: [u32; 3],
        residual_buf: &wgpu::Buffer,
        bounds_buf: &wgpu::Buffer,
        w2_buf: &wgpu::Buffer,
        numden_buf: &wgpu::Buffer,
    ) -> GpuResult<()> {
        self.run(
            "n4 fit",
            &self.kernels.fit_gather,
            &[
                (3, bounds_buf),
                (4, w2_buf),
                (5, residual_buf),
                (6, numden_buf),
                (7, uniform),
            ],
            groups,
        )
    }

    fn fit_args(&self, plan: &FitPlan) -> (FitU, [u32; 3]) {
        let n = plan.grid_size[0] * plan.grid_size[1] * plan.grid_size[2];
        let cp = plan.bounds.len() as u32;
        let slabs = fit_slabs(cp);
        let (groups, stride) = self.flat_groups(cp * slabs);
        let un = FitU {
            dims: [
                plan.grid_size[0] as u32,
                plan.grid_size[1] as u32,
                plan.grid_size[2] as u32,
                n as u32,
            ],
            lat: [
                plan.lattice_size[0] as u32,
                plan.lattice_size[1] as u32,
                plan.lattice_size[2] as u32,
                plan.order as u32,
            ],
            scale: [
                plan.scale[0] as f32,
                plan.scale[1] as f32,
                plan.scale[2] as f32,
                0.5 * (plan.order as f32 - 1.0),
            ],
            spans: [
                (plan.lattice_size[0] - plan.order) as f32,
                (plan.lattice_size[1] - plan.order) as f32,
                (plan.lattice_size[2] - plan.order) as f32,
                0.0,
            ],
            disp: [stride, slabs, 0, 0],
        };
        (un, groups)
    }

    /// Runs N4 on `input` and returns the corrected image, the bias field and the level traces.
    pub fn run_n4(&self, input: &Volume<f64>, params: &N4Params) -> GpuResult<N4Outcome> {
        self.run_n4_cancellable(input, params, None)
    }

    /// Like [`Self::run_n4`]; `cancel` is checked once per fitting iteration and a set flag ends
    /// the run with [`GpuError::Cancelled`].
    pub fn run_n4_cancellable(
        &self,
        input: &Volume<f64>,
        params: &N4Params,
        cancel: Option<&AtomicBool>,
    ) -> GpuResult<N4Outcome> {
        params.validate().map_err(reg)?;
        let full_grid = *input.grid();
        let f = params.shrink_factor.max(1);
        let shrunk = shrink_volume(input, [f, f, f]).map_err(reg)?;
        let ssize = shrunk.grid().size();
        let n_s = ssize[0] * ssize[1] * ssize[2];
        let log_in: Vec<f32> = shrunk
            .data()
            .iter()
            .map(|v| if *v > 0.0 { v.ln() as f32 } else { *v as f32 })
            .collect();
        let log_in_buf = self.storage("n4 log input", n_s as u64 * 4)?;
        self.upload(&log_in_buf, &log_in);
        let log_bias_buf = self.storage("n4 log bias", n_s as u64 * 4)?;
        let log_unc_buf = self.storage("n4 log uncorrected", n_s as u64 * 4)?;
        let residual_buf = self.storage("n4 residual", n_s as u64 * 4)?;
        let (log_groups, log_stride) = self.flat_groups(n_s as u32);
        let lu_buf = self.ctx.uniform(
            "n4 log",
            &LogU {
                a: [n_s as u32, log_stride, 0, 0],
                b: [0; 4],
            },
        )?;
        let tu_buf = self.ctx.uniform(
            "n4 lut",
            &LutU {
                a: [
                    n_s as u32,
                    params.number_of_histogram_bins as u32,
                    log_stride,
                    0,
                ],
                f: [0.0; 4],
            },
        )?;
        let mut table_slot: Option<Grown> = None;
        let mut lat_slot: Option<Grown> = None;
        let mut staging_slot: Option<Grown> = None;

        let mut lattice: Option<ControlLattice> = None;
        let mut log_bias = vec![0.0f64; n_s];
        let mut levels = Vec::with_capacity(params.fitting_levels);
        for level in 0..params.fitting_levels {
            let cp = lattice
                .as_ref()
                .map(|l| l.size)
                .unwrap_or(params.number_of_control_points);
            let plan = FitPlan::new(ssize, cp, params.spline_order).map_err(reg)?;
            let bounds_u32: Vec<u32> = plan.bounds.iter().flatten().copied().collect();
            let w2_f32: Vec<f32> = plan.w2sum.iter().map(|v| *v as f32).collect();
            let bounds_buf = self.storage("n4 bounds", bounds_u32.len() as u64 * 4)?;
            self.upload(&bounds_buf, &bounds_u32);
            let w2_buf = self.storage("n4 w2sum", w2_f32.len() as u64 * 4)?;
            self.upload(&w2_buf, &w2_f32);
            let slabs = fit_slabs(plan.bounds.len() as u32) as usize;
            let numden_len = 2 * plan.bounds.len() * slabs;
            let numden_buf = self.storage("n4 numden", numden_len as u64 * 4)?;
            let (fit_un, fit_groups) = self.fit_args(&plan);
            let fit_buf = self.ctx.uniform("n4 fit", &fit_un)?;
            let (eval_un, eval_groups) = self.eval_args(cp, ssize);
            let eval_buf = self.ctx.uniform("n4 eval", &eval_un)?;
            let lat_bytes = (cp[0] * cp[1] * cp[2]) as u64 * 4;
            self.ensure(&mut lat_slot, "n4 lattice", lat_bytes, false)?;
            let staging_bytes = (n_s as u64 * 4).max(4 * numden_len as u64);
            self.ensure(&mut staging_slot, "readback staging", staging_bytes, true)?;

            let limit = params.iterations[level];
            let mut trace = Vec::with_capacity(limit);
            let mut executed = 0usize;
            for iteration in 0..limit {
                if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
                    return Err(GpuError::Cancelled);
                }
                executed += 1;
                if iteration == 0 {
                    let old: Vec<f32> = log_bias.iter().map(|v| *v as f32).collect();
                    self.upload(&log_bias_buf, &old);
                }
                self.run(
                    "n4 log",
                    &self.kernels.log_residual,
                    &[
                        (8, &log_in_buf),
                        (9, &log_bias_buf),
                        (10, &log_unc_buf),
                        (11, &lu_buf),
                    ],
                    log_groups,
                )?;
                let unc = self
                    .ctx
                    .read_f32_staged(&log_unc_buf, n_s, Self::slot(&staging_slot)?)
                    .map_err(|e| GpuError::Readback(e.to_string()))?;
                let unc64: Vec<f64> = unc.iter().map(|v| f64::from(*v)).collect();
                let lut = sharpen_lut(
                    &unc64,
                    params.number_of_histogram_bins,
                    params.bias_field_fwhm,
                    params.wiener_filter_noise,
                )
                .map_err(reg)?;
                let table: Vec<f32> = lut.table.iter().map(|v| *v as f32).collect();
                self.ensure(&mut table_slot, "n4 table", table.len() as u64 * 4, false)?;
                let table_buf = Self::slot(&table_slot)?;
                self.upload(table_buf, &table);
                let tu = LutU {
                    a: [n_s as u32, table.len() as u32, log_stride, 0],
                    f: [lut.bin_min as f32, lut.slope as f32, 0.0, 0.0],
                };
                self.upload(&tu_buf, std::slice::from_ref(&tu));
                self.run(
                    "n4 lut",
                    &self.kernels.apply_lut,
                    &[
                        (10, &log_unc_buf),
                        (12, table_buf),
                        (13, &residual_buf),
                        (14, &tu_buf),
                    ],
                    log_groups,
                )?;
                self.run_fit(
                    &fit_buf,
                    fit_groups,
                    &residual_buf,
                    &bounds_buf,
                    &w2_buf,
                    &numden_buf,
                )?;
                let nd = self
                    .ctx
                    .read_f32_staged(&numden_buf, numden_len, Self::slot(&staging_slot)?)
                    .map_err(|e| GpuError::Readback(e.to_string()))?;
                let phi = phi_from_partials(&nd, plan.bounds.len(), slabs);
                let phi = ControlLattice::from_vec(cp, phi).map_err(reg)?;
                lattice = Some(match lattice.take() {
                    None => phi,
                    Some(existing) => existing.add(&phi).map_err(reg)?,
                });
                let lat = lattice
                    .as_ref()
                    .ok_or_else(|| GpuError::Internal("no lattice".into()))?;
                let lat_f32: Vec<f32> = lat.data.iter().map(|v| *v as f32).collect();
                let lat_buf = Self::slot(&lat_slot)?;
                self.upload(lat_buf, &lat_f32);
                self.run_eval(&eval_buf, eval_groups, &log_bias_buf, lat_buf)?;
                let new = self
                    .ctx
                    .read_f32_staged(&log_bias_buf, n_s, Self::slot(&staging_slot)?)
                    .map_err(|e| GpuError::Readback(e.to_string()))?;
                let new64: Vec<f64> = new.iter().map(|v| f64::from(*v)).collect();
                let convergence = convergence_measurement(&log_bias, &new64);
                trace.push(convergence);
                log_bias = new64;
                if convergence <= params.convergence_threshold {
                    break;
                }
            }
            levels.push(N4LevelTrace {
                level,
                iterations: executed,
                control_points: cp,
                grid_size: ssize,
                convergence: trace,
            });
            if level + 1 < params.fitting_levels {
                let current = lattice
                    .as_ref()
                    .ok_or_else(|| GpuError::Internal("no lattice".into()))?;
                lattice = Some(refine_lattice(current, params.spline_order).map_err(reg)?);
            }
        }
        let lattice = lattice.ok_or_else(|| GpuError::Internal("no lattice".into()))?;

        let full_size = full_grid.size();
        let n_full = full_grid.num_voxels();
        let input_full: Vec<f32> = input.data().iter().map(|v| *v as f32).collect();
        let input_buf = self.storage("n4 full input", n_full as u64 * 4)?;
        self.upload(&input_buf, &input_full);
        let lat_f32: Vec<f32> = lattice.data.iter().map(|v| *v as f32).collect();
        let lat_buf = self.storage("n4 final lattice", lat_f32.len() as u64 * 4)?;
        self.upload(&lat_buf, &lat_f32);
        let log_full_buf = self.storage("n4 log bias full", n_full as u64 * 4)?;
        self.eval(&lattice, full_size, &log_full_buf, &lat_buf)?;
        let corrected_buf = self.storage("n4 corrected", n_full as u64 * 4)?;
        let bias_buf = self.storage("n4 bias field", n_full as u64 * 4)?;
        let (div_groups, div_stride) = self.flat_groups(n_full as u32);
        let du = DivU {
            a: [n_full as u32, div_stride, 0, 0],
            b: [0; 4],
        };
        let du_buf = self.ctx.uniform("n4 divide", &du)?;
        self.run(
            "n4 divide",
            &self.kernels.divide_exp,
            &[
                (15, &input_buf),
                (16, &log_full_buf),
                (17, &corrected_buf),
                (18, &bias_buf),
                (19, &du_buf),
            ],
            div_groups,
        )?;
        let corrected = self
            .ctx
            .read_f32(&corrected_buf, n_full)
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let bias = self
            .ctx
            .read_f32(&bias_buf, n_full)
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        Ok(N4Outcome {
            corrected: Volume::from_vec(full_grid, corrected).map_err(reg)?,
            bias_field: Volume::from_vec(full_grid, bias).map_err(reg)?,
            lattice,
            levels,
        })
    }

    fn selftest(&mut self) -> GpuResult<()> {
        let mut record = N4SelfTestRecord::default();
        for family in N4Family::ALL {
            let (attempts, line, first) = self.selftest_family(family)?;
            record.attempts = record.attempts.max(attempts);
            match family {
                N4Family::Eval => record.eval_attempts = attempts,
                N4Family::Fit => record.fit_attempts = attempts,
                N4Family::Log => record.log_attempts = attempts,
                N4Family::Lut => record.lut_attempts = attempts,
                N4Family::Div => record.div_attempts = attempts,
            }
            record.lines.push(line);
            if let Some(failure) = first {
                record.first_attempt_failures.push(failure);
            }
        }
        self.selftest = record;
        Ok(())
    }

    fn selftest_family(&mut self, family: N4Family) -> GpuResult<(u32, String, Option<String>)> {
        let mut first: Option<String> = None;
        for attempt in 1..=SELFTEST_ATTEMPTS {
            match self.check_family_both_dispatch_shapes(family) {
                Ok(line) => return Ok((attempt, line, first)),
                Err(e) => {
                    let message = e.to_string();
                    if first.is_none() {
                        first = Some(message.clone());
                    }
                    if attempt == SELFTEST_ATTEMPTS {
                        return Err(GpuError::Verification(format!(
                            "known-answer check of the N4 {} kernel failed on {} / {} in \
                             {SELFTEST_ATTEMPTS} attempts with freshly compiled kernels: {message}",
                            family.label(),
                            self.ctx.info.name,
                            self.ctx.info.backend
                        )));
                    }
                    self.recompile(family)?;
                }
            }
        }
        Err(GpuError::Internal("n4 selftest loop fell through".into()))
    }

    fn recompile(&mut self, family: N4Family) -> GpuResult<()> {
        match family {
            N4Family::Eval => self.kernels.eval_lattice = N4Kernels::compile_eval(&self.ctx)?,
            N4Family::Fit => self.kernels.fit_gather = N4Kernels::compile_fit(&self.ctx)?,
            N4Family::Log => self.kernels.log_residual = N4Kernels::compile_log(&self.ctx)?,
            N4Family::Lut => self.kernels.apply_lut = N4Kernels::compile_lut(&self.ctx)?,
            N4Family::Div => self.kernels.divide_exp = N4Kernels::compile_div(&self.ctx)?,
        }
        Ok(())
    }

    fn check_family_both_dispatch_shapes(&mut self, family: N4Family) -> GpuResult<String> {
        let line = self.check_family(family)?;
        let full = self.max_groups;
        self.max_groups = 1;
        let capped = self.check_family(family);
        self.max_groups = full;
        Ok(format!("{line}; one-group dispatch rows: {}", capped?))
    }

    fn check_family(&self, family: N4Family) -> GpuResult<String> {
        match family {
            N4Family::Eval => self.check_eval(),
            N4Family::Fit => self.check_fit(),
            N4Family::Log => self.check_log(),
            N4Family::Lut => self.check_lut(),
            N4Family::Div => self.check_div(),
        }
    }

    fn check_eval(&self) -> GpuResult<String> {
        let cpu_lat = ControlLattice::from_vec(
            [5, 5, 5],
            (0..125).map(|v| (v as f64 * 0.17).sin()).collect(),
        )
        .map_err(reg)?;
        let out_size = [13usize, 11, 9];
        let n_out = out_size[0] * out_size[1] * out_size[2];
        let lat_f32: Vec<f32> = cpu_lat.data.iter().map(|v| *v as f32).collect();
        let lat_buf = self.storage("n4 selftest lattice", lat_f32.len() as u64 * 4)?;
        self.upload(&lat_buf, &lat_f32);
        let out_buf = self.storage("n4 selftest out", n_out as u64 * 4)?;
        self.eval(&cpu_lat, out_size, &out_buf, &lat_buf)?;
        let got = self
            .ctx
            .read_f32(&out_buf, n_out)
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let want = eval_lattice(&cpu_lat, out_size, N4_SPLINE_ORDER).map_err(reg)?;
        let rel = max_rel(&got, &want);
        if rel > 1e-4 {
            return Err(GpuError::Verification(format!(
                "N4 eval_lattice known-answer: relative error {rel:.3e}"
            )));
        }
        Ok(format!(
            "N4 eval_lattice known-answer: relative error {rel:.3e}"
        ))
    }

    fn check_fit(&self) -> GpuResult<String> {
        let gsize = [13usize, 11, 9];
        let plan = FitPlan::new(gsize, [5, 5, 5], N4_SPLINE_ORDER).map_err(reg)?;
        let residual: Vec<f64> = (0..plan.w2sum.len())
            .map(|lin| 0.3 * ((lin % 7) as f64) - 0.2 * ((lin % 5) as f64))
            .collect();
        let cpu_phi = plan.phi(&residual).map_err(reg)?;
        let bounds_u32: Vec<u32> = plan.bounds.iter().flatten().copied().collect();
        let w2_f32: Vec<f32> = plan.w2sum.iter().map(|v| *v as f32).collect();
        let residual_f32: Vec<f32> = residual.iter().map(|v| *v as f32).collect();
        let bounds_buf = self.storage("n4 selftest bounds", bounds_u32.len() as u64 * 4)?;
        self.upload(&bounds_buf, &bounds_u32);
        let w2_buf = self.storage("n4 selftest w2", w2_f32.len() as u64 * 4)?;
        self.upload(&w2_buf, &w2_f32);
        let res_buf = self.storage("n4 selftest residual", residual_f32.len() as u64 * 4)?;
        self.upload(&res_buf, &residual_f32);
        let slabs = fit_slabs(plan.bounds.len() as u32) as usize;
        let numden_len = 2 * plan.bounds.len() * slabs;
        let numden_buf = self.storage("n4 selftest numden", numden_len as u64 * 4)?;
        self.fit(&plan, &res_buf, &bounds_buf, &w2_buf, &numden_buf)?;
        let nd = self
            .ctx
            .read_f32(&numden_buf, numden_len)
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let got_phi = phi_from_partials(&nd, plan.bounds.len(), slabs);
        let range = cpu_phi
            .data
            .iter()
            .fold(0.0f64, |a, b| a.max(b.abs()))
            .max(1e-9);
        let mut worst = 0.0f64;
        for (got, want) in got_phi.iter().zip(&cpu_phi.data) {
            worst = worst.max((got - want).abs());
        }
        let rel = worst / range;
        if rel > 1e-4 {
            return Err(GpuError::Verification(format!(
                "N4 fit_gather known-answer: relative error {rel:.3e}"
            )));
        }
        Ok(format!(
            "N4 fit_gather known-answer: relative error {rel:.3e}"
        ))
    }

    fn check_log(&self) -> GpuResult<String> {
        let n = 3 * WG as usize + 7;
        let log_in: Vec<f32> = (0..n).map(|i| 0.5 + i as f32 * 0.07).collect();
        let log_bias: Vec<f32> = (0..n).map(|i| -0.2 + i as f32 * 0.03).collect();
        let want: Vec<f64> = log_in
            .iter()
            .zip(&log_bias)
            .map(|(a, b)| f64::from(*a) - f64::from(*b))
            .collect();
        let in_buf = self.storage("n4 selftest log_in", n as u64 * 4)?;
        self.upload(&in_buf, &log_in);
        let bias_buf = self.storage("n4 selftest log_bias", n as u64 * 4)?;
        self.upload(&bias_buf, &log_bias);
        let out_buf = self.storage("n4 selftest log_out", n as u64 * 4)?;
        let (groups, stride) = self.flat_groups(n as u32);
        let lu = LogU {
            a: [n as u32, stride, 0, 0],
            b: [0; 4],
        };
        let lu_buf = self.ctx.uniform("n4 selftest log", &lu)?;
        self.run(
            "n4 selftest log",
            &self.kernels.log_residual,
            &[(8, &in_buf), (9, &bias_buf), (10, &out_buf), (11, &lu_buf)],
            groups,
        )?;
        let got = self
            .ctx
            .read_f32(&out_buf, n)
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let rel = max_rel(&got, &want);
        if rel > 1e-5 {
            return Err(GpuError::Verification(format!(
                "N4 log_residual known-answer: relative error {rel:.3e}"
            )));
        }
        Ok(format!(
            "N4 log_residual known-answer: relative error {rel:.3e}"
        ))
    }

    fn check_lut(&self) -> GpuResult<String> {
        let bins = 16usize;
        let bin_min = -3.0f32;
        let slope = 0.5f32;
        let table: Vec<f32> = (0..bins)
            .map(|i| -1.5 + i as f32 * 0.23 + (i as f32 * 0.7).sin() * 0.1)
            .collect();
        let lut = SharpenLut {
            bin_min: f64::from(bin_min),
            bin_max: f64::from(bin_min) + f64::from(slope) * (bins - 1) as f64,
            slope: f64::from(slope),
            table: table.iter().map(|v| f64::from(*v)).collect(),
        };
        let n = 3 * WG as usize + 5;
        let step = 3.6 / n as f32;
        let values: Vec<f32> = (0..n).map(|i| bin_min - 0.6 + i as f32 * step).collect();
        let want: Vec<f64> = values
            .iter()
            .map(|v| f64::from(*v) - apply_sharpen_lut(&lut, f64::from(*v)))
            .collect();
        let value_buf = self.storage("n4 selftest lut_in", n as u64 * 4)?;
        self.upload(&value_buf, &values);
        let table_buf = self.storage("n4 selftest table", table.len() as u64 * 4)?;
        self.upload(&table_buf, &table);
        let out_buf = self.storage("n4 selftest lut_out", n as u64 * 4)?;
        let (groups, stride) = self.flat_groups(n as u32);
        let tu = LutU {
            a: [n as u32, bins as u32, stride, 0],
            f: [bin_min, slope, 0.0, 0.0],
        };
        let tu_buf = self.ctx.uniform("n4 selftest lut", &tu)?;
        self.run(
            "n4 selftest lut",
            &self.kernels.apply_lut,
            &[
                (10, &value_buf),
                (12, &table_buf),
                (13, &out_buf),
                (14, &tu_buf),
            ],
            groups,
        )?;
        let got = self
            .ctx
            .read_f32(&out_buf, n)
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let rel = max_rel(&got, &want);
        if rel > 1e-4 {
            return Err(GpuError::Verification(format!(
                "N4 apply_lut known-answer: relative error {rel:.3e}"
            )));
        }
        Ok(format!(
            "N4 apply_lut known-answer: relative error {rel:.3e}"
        ))
    }

    fn check_div(&self) -> GpuResult<String> {
        let n = 3 * WG as usize + 9;
        let input: Vec<f32> = (0..n).map(|i| 10.0 + i as f32 * 3.7).collect();
        let log_bias: Vec<f32> = (0..n).map(|i| -0.3 + i as f32 * 0.021).collect();
        let want_field: Vec<f64> = log_bias.iter().map(|v| f64::from(*v).exp()).collect();
        let want_corrected: Vec<f64> = input
            .iter()
            .zip(&want_field)
            .map(|(v, f)| f64::from(*v) / f)
            .collect();
        let input_buf = self.storage("n4 selftest div_in", n as u64 * 4)?;
        self.upload(&input_buf, &input);
        let log_buf = self.storage("n4 selftest div_log", n as u64 * 4)?;
        self.upload(&log_buf, &log_bias);
        let corrected_buf = self.storage("n4 selftest div_corrected", n as u64 * 4)?;
        let bias_buf = self.storage("n4 selftest div_bias", n as u64 * 4)?;
        let (groups, stride) = self.flat_groups(n as u32);
        let du = DivU {
            a: [n as u32, stride, 0, 0],
            b: [0; 4],
        };
        let du_buf = self.ctx.uniform("n4 selftest div", &du)?;
        self.run(
            "n4 selftest div",
            &self.kernels.divide_exp,
            &[
                (15, &input_buf),
                (16, &log_buf),
                (17, &corrected_buf),
                (18, &bias_buf),
                (19, &du_buf),
            ],
            groups,
        )?;
        let corrected = self
            .ctx
            .read_f32(&corrected_buf, n)
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let bias = self
            .ctx
            .read_f32(&bias_buf, n)
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let rel = max_rel(&corrected, &want_corrected).max(max_rel(&bias, &want_field));
        if rel > 1e-4 {
            return Err(GpuError::Verification(format!(
                "N4 divide_exp known-answer: relative error {rel:.3e}"
            )));
        }
        Ok(format!(
            "N4 divide_exp known-answer: relative error {rel:.3e}"
        ))
    }
}

/// z slabs each control point's support box is split into so that the fit has enough invocations
/// (the coarsest level has a few dozen control points, each with a box of most of the grid).
fn fit_slabs(control_points: u32) -> u32 {
    FIT_TARGET_INVOCATIONS
        .div_ceil(control_points.max(1))
        .clamp(1, FIT_MAX_SLABS)
}

/// Sums the per-slab `(num, den)` partials of every control point in slab order, in `f64`, and
/// divides: the fixed order keeps the result deterministic.
fn phi_from_partials(partials: &[f32], control_points: usize, slabs: usize) -> Vec<f64> {
    (0..control_points)
        .map(|c| {
            let (mut num, mut den) = (0.0f64, 0.0f64);
            for k in 0..slabs {
                let t = c * slabs + k;
                num += f64::from(partials[2 * t]);
                den += f64::from(partials[2 * t + 1]);
            }
            if den > 0.0 {
                num / den
            } else {
                0.0
            }
        })
        .collect()
}

fn flat_groups_with(items: u32, max_groups: u32) -> ([u32; 3], u32) {
    let groups = items.div_ceil(WG).max(1);
    let max = max_groups.max(1);
    if groups <= max {
        return ([groups, 1, 1], 0);
    }
    let gx = max;
    let gy = groups.div_ceil(gx);
    ([gx, gy, 1], gx * WG)
}

fn max_rel(got: &[f32], want: &[f64]) -> f64 {
    let range = want.iter().fold(0.0f64, |a, b| a.max(b.abs())).max(1e-9);
    got.iter()
        .zip(want)
        .fold(0.0f64, |a, (g, w)| a.max((f64::from(*g) - w).abs()))
        / range
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fit_splits_small_lattices_into_slabs_and_sums_them_in_order() {
        assert_eq!(fit_slabs(64), 64);
        assert_eq!(fit_slabs(125), 33);
        assert_eq!(fit_slabs(4096), 1);
        assert_eq!(fit_slabs(100_000), 1);
        let partials = [1.0f32, 2.0, 3.0, 4.0, 5.0, 0.0, 7.0, 0.0];
        assert_eq!(phi_from_partials(&partials, 2, 2), vec![4.0 / 6.0, 0.0]);
    }

    #[test]
    fn flat_groups_wrap_into_rows_past_the_cap() {
        assert_eq!(flat_groups_with(10, 65535), ([1, 1, 1], 0));
        assert_eq!(flat_groups_with(3 * WG + 1, 1), ([1, 4, 1], WG));
        assert_eq!(flat_groups_with(5 * WG, 2), ([2, 3, 1], 2 * WG));
    }
}
