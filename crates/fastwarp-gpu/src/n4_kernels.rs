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
// Portions derived from ITK `itkBSplineControlPointImageFilter.hxx` and
// `itkBSplineScatteredDataPointSetToImageFilter.hxx` (the N4 B-spline evaluation and fit) plus
// `itkN4BiasFieldCorrectionImageFilter.hxx` (the sharpening lookup and `input / exp(field)`). See
// `docs/THIRD_PARTY_PORTS.md`.

//! Compiled pipelines and uniform layouts of the N4 kernels (`shaders/n4.wgsl`).

use bytemuck::{Pod, Zeroable};

use crate::device::GpuContext;
use crate::error::GpuResult;

const SRC: &str = include_str!("shaders/n4.wgsl");

/// Uniform of `k_n4_eval_lattice`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct EvalU {
    /// `[cp_x, cp_y, cp_z, order]`.
    pub lat: [u32; 4],
    /// `[nx, ny, nz, n]` of the evaluation grid.
    pub outd: [u32; 4],
    /// `[scale_x, scale_y, scale_z, 0.5 * (order - 1)]`.
    pub scale: [f32; 4],
    /// `[x_stride, 0, 0, 0]`: invocations added per workgroup row.
    pub disp: [u32; 4],
}

/// Uniform of `k_n4_fit_gather`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct FitU {
    /// `[nx, ny, nz, n]` of the fitting grid.
    pub dims: [u32; 4],
    /// `[cp_x, cp_y, cp_z, order]`.
    pub lat: [u32; 4],
    /// `[scale_x, scale_y, scale_z, 0.5 * (order - 1)]`.
    pub scale: [f32; 4],
    /// `[spans_x, spans_y, spans_z, 0]`.
    pub spans: [f32; 4],
    /// `[x_stride, slabs, 0, 0]`: invocations added per workgroup row, and the number of z slabs
    /// each control point's support box is split into (one invocation per control point and slab).
    pub disp: [u32; 4],
}

/// Uniform of `k_n4_log_residual_hist`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct LogU {
    /// `[n, row_stride, 0, 0]`: `row_stride` is the invocations added per workgroup row.
    pub a: [u32; 4],
    /// Padding to the minimum uniform binding size.
    pub b: [u32; 4],
}

/// Uniform of `k_n4_apply_lut`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct LutU {
    /// `[n, bins, row_stride, 0]`.
    pub a: [u32; 4],
    /// `[bin_min, slope, 0, 0]`.
    pub f: [f32; 4],
}

/// Uniform of `k_n4_divide_exp`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct DivU {
    /// `[n, row_stride, 0, 0]`.
    pub a: [u32; 4],
    /// Padding to the minimum uniform binding size.
    pub b: [u32; 4],
}

/// The compiled N4 pipelines.
pub struct N4Kernels {
    /// Full-resolution lattice evaluation.
    pub eval_lattice: wgpu::ComputePipeline,
    /// Per-control-point gathered fit.
    pub fit_gather: wgpu::ComputePipeline,
    /// `log_input - log_bias`.
    pub log_residual: wgpu::ComputePipeline,
    /// Apply the sharpening table.
    pub apply_lut: wgpu::ComputePipeline,
    /// `input / exp(field)` and the bias field.
    pub divide_exp: wgpu::ComputePipeline,
}

impl N4Kernels {
    /// Compiles every N4 kernel; shader and pipeline errors are returned verbatim.
    pub fn new(ctx: &GpuContext) -> GpuResult<Self> {
        Ok(N4Kernels {
            eval_lattice: Self::compile_eval(ctx)?,
            fit_gather: Self::compile_fit(ctx)?,
            log_residual: Self::compile_log(ctx)?,
            apply_lut: Self::compile_lut(ctx)?,
            divide_exp: Self::compile_div(ctx)?,
        })
    }

    /// Compiles only `k_n4_eval_lattice` (a fresh pipeline for the known-answer retry).
    pub fn compile_eval(ctx: &GpuContext) -> GpuResult<wgpu::ComputePipeline> {
        ctx.compile("k_n4_eval_lattice", SRC, "k_n4_eval_lattice")
    }

    /// Compiles only `k_n4_fit_gather`.
    pub fn compile_fit(ctx: &GpuContext) -> GpuResult<wgpu::ComputePipeline> {
        ctx.compile("k_n4_fit_gather", SRC, "k_n4_fit_gather")
    }

    /// Compiles only `k_n4_log_residual_hist`.
    pub fn compile_log(ctx: &GpuContext) -> GpuResult<wgpu::ComputePipeline> {
        ctx.compile("k_n4_log_residual_hist", SRC, "k_n4_log_residual_hist")
    }

    /// Compiles only `k_n4_apply_lut`.
    pub fn compile_lut(ctx: &GpuContext) -> GpuResult<wgpu::ComputePipeline> {
        ctx.compile("k_n4_apply_lut", SRC, "k_n4_apply_lut")
    }

    /// Compiles only `k_n4_divide_exp`.
    pub fn compile_div(ctx: &GpuContext) -> GpuResult<wgpu::ComputePipeline> {
        ctx.compile("k_n4_divide_exp", SRC, "k_n4_divide_exp")
    }
}
