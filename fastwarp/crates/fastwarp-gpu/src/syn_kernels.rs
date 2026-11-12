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
// Portions derived from ITK `itkSyNImageRegistrationMethod.hxx` and
// `itkANTSNeighborhoodCorrelationImageToImageMetricv4GetValueAndDerivativeThreader.hxx`
// (the host-side pipeline and uniform layouts of the SyN kernels). See
// `docs/THIRD_PARTY_PORTS.md`.

//! Compiled pipelines and uniform layouts of the SyN kernels.

use bytemuck::{Pod, Zeroable};

use crate::device::GpuContext;
use crate::error::GpuResult;

const SAMPLE: &str = include_str!("shaders/syn_sample.wgsl");
const WARP: &str = include_str!("shaders/syn_warp.wgsl");
const COMPOSE: &str = include_str!("shaders/syn_compose.wgsl");
const INV_RESIDUAL: &str = include_str!("shaders/syn_inv_residual.wgsl");
const INV_BEGIN: &str = include_str!("shaders/syn_inv_begin.wgsl");
const INV_REDUCE: &str = include_str!("shaders/syn_inv_reduce.wgsl");
const INV_UPDATE: &str = include_str!("shaders/syn_inv_update.wgsl");
const CC_PLANES: &str = include_str!("shaders/syn_cc_planes.wgsl");
const CC_DERIV: &str = include_str!("shaders/syn_cc_deriv.wgsl");
const CC_VALUE: &str = include_str!("shaders/syn_cc_value.wgsl");
const GAUSS: &str = include_str!("shaders/syn_gauss.wgsl");
const NORM_PARTIAL: &str = include_str!("shaders/syn_norm_partial.wgsl");
const REDUCE_SCALAR: &str = include_str!("shaders/syn_reduce_scalar.wgsl");
const SCALE: &str = include_str!("shaders/syn_scale.wgsl");
const AXPY: &str = include_str!("shaders/syn_axpy.wgsl");
const BOX: &str = include_str!("shaders/box.wgsl");

/// Uniform of `k_warp`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct WarpU {
    pub dims: [u32; 4],
    pub src_dims: [u32; 4],
    pub flags: u32,
    pub nplanes: u32,
    pub pad: [u32; 2],
    pub mi: [[f32; 4]; 3],
    pub l: [[f32; 4]; 3],
}

/// Uniform of `k_compose`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct ComposeU {
    pub dims: [u32; 4],
    pub l: [[f32; 4]; 3],
}

/// Uniform of `k_inv_residual`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct InvResidualU {
    pub dims: [u32; 4],
    pub l: [[f32; 4]; 3],
    pub sp: [f32; 4],
}

/// Uniform of `k_inv_begin`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct InvBeginU {
    pub max_tol: f32,
    pub mean_tol: f32,
    pub pad: [u32; 2],
}

/// Uniform of `k_inv_reduce`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct InvReduceU {
    pub count: u32,
    pub n: u32,
    pub pad: [u32; 2],
}

/// Uniform of `k_inv_update`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct InvUpdateU {
    pub dims: [u32; 4],
    pub sp: [f32; 4],
    pub eps: f32,
    pub enforce: u32,
    pub pad: [u32; 2],
}

/// Uniform of `k_cc_planes`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct CcPlanesU {
    pub n: u32,
    pub pad: [u32; 3],
}

/// Uniform of the CC derivative and value kernels (`k_cc_deriv`, `k_cc_value`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct CcFinalU {
    pub dims: [u32; 4],
    pub radius: u32,
    pub part_base: u32,
    pub pad: [u32; 2],
    pub o: [[f32; 4]; 3],
    pub sp: [f32; 4],
}

/// Uniform of `k_gauss_axis`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GaussU {
    pub dims: [u32; 4],
    pub axis: u32,
    pub len: u32,
    pub stride: u32,
    pub radius: u32,
    pub tap_off: u32,
    pub fin: u32,
    pub other_stride: u32,
    pub pad: u32,
    pub w1: f32,
    pub w2: f32,
    pub pad_f: [f32; 2],
}

/// Uniform of `k_norm_partial`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct NormU {
    pub n: u32,
    pub mode: u32,
    pub pad: [u32; 2],
    pub sp: [f32; 4],
}

/// Uniform of `k_reduce_scalar`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct ReduceU {
    pub count: u32,
    pub mode: u32,
    pub out_index: u32,
    pub pad: u32,
}

/// Uniform of `k_scale` and `k_axpy_field`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct ElementU {
    pub total: u32,
    pub a: f32,
    pub pad: [u32; 2],
}

/// The compiled SyN pipelines.
pub struct SynKernels {
    pub warp: wgpu::ComputePipeline,
    pub compose: wgpu::ComputePipeline,
    pub inv_residual: wgpu::ComputePipeline,
    pub inv_begin: wgpu::ComputePipeline,
    pub inv_reduce: wgpu::ComputePipeline,
    pub inv_update: wgpu::ComputePipeline,
    pub cc_planes: wgpu::ComputePipeline,
    pub cc_deriv: wgpu::ComputePipeline,
    pub cc_value: wgpu::ComputePipeline,
    pub box_x: wgpu::ComputePipeline,
    pub box_yz: wgpu::ComputePipeline,
    pub gauss: wgpu::ComputePipeline,
    pub gauss_x: wgpu::ComputePipeline,
    pub gauss_yz: wgpu::ComputePipeline,
    pub norm_partial: wgpu::ComputePipeline,
    pub reduce_scalar: wgpu::ComputePipeline,
    pub scale: wgpu::ComputePipeline,
    pub axpy: wgpu::ComputePipeline,
}

impl SynKernels {
    /// Compiles every SyN kernel; shader and pipeline errors are returned verbatim.
    pub fn new(ctx: &GpuContext) -> GpuResult<Self> {
        let with_sample = |body: &str| format!("{SAMPLE}\n{body}");
        Ok(SynKernels {
            warp: ctx.compile("k_warp", &with_sample(WARP), "k_warp")?,
            compose: ctx.compile("k_compose", &with_sample(COMPOSE), "k_compose")?,
            inv_residual: ctx.compile(
                "k_inv_residual",
                &with_sample(INV_RESIDUAL),
                "k_inv_residual",
            )?,
            inv_begin: ctx.compile("k_inv_begin", INV_BEGIN, "k_inv_begin")?,
            inv_reduce: ctx.compile("k_inv_reduce", INV_REDUCE, "k_inv_reduce")?,
            inv_update: ctx.compile("k_inv_update", INV_UPDATE, "k_inv_update")?,
            cc_planes: ctx.compile("k_cc_planes", CC_PLANES, "k_cc_planes")?,
            cc_deriv: ctx.compile("k_cc_deriv", &with_sample(CC_DERIV), "k_cc_deriv")?,
            cc_value: ctx.compile("k_cc_value", CC_VALUE, "k_cc_value")?,
            box_x: ctx.compile("k_box_x", BOX, "k_box_x")?,
            box_yz: ctx.compile("k_box_yz", BOX, "k_box_yz")?,
            gauss: ctx.compile("k_gauss_axis", GAUSS, "k_gauss_axis")?,
            gauss_x: ctx.compile("k_gauss_x", GAUSS, "k_gauss_x")?,
            gauss_yz: ctx.compile("k_gauss_yz", GAUSS, "k_gauss_yz")?,
            norm_partial: ctx.compile("k_norm_partial", NORM_PARTIAL, "k_norm_partial")?,
            reduce_scalar: ctx.compile("k_reduce_scalar", REDUCE_SCALAR, "k_reduce_scalar")?,
            scale: ctx.compile("k_scale", SCALE, "k_scale")?,
            axpy: ctx.compile("k_axpy_field", AXPY, "k_axpy_field")?,
        })
    }
}
