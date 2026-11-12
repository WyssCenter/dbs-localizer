// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;

use bytemuck::{Pod, Zeroable};

use crate::device::GpuContext;
use crate::error::GpuResult;

#[cfg(feature = "dev-tools")]
const SRC_AXPY: &str = include_str!("shaders/axpy.wgsl");
#[cfg(feature = "dev-tools")]
const SRC_NOOP: &str = include_str!("shaders/noop.wgsl");
#[cfg(feature = "dev-tools")]
const SRC_CC: &str = include_str!("shaders/cc.wgsl");
#[cfg(feature = "dev-tools")]
const SRC_CC_DERIV: &str = include_str!("shaders/cc_deriv.wgsl");
#[cfg(feature = "dev-tools")]
const SRC_REDUCE: &str = include_str!("shaders/reduce.wgsl");
#[cfg(feature = "dev-tools")]
const SRC_BOX: &str = include_str!("shaders/box.wgsl");
#[cfg(feature = "dev-tools")]
const SRC_WARP: &str = include_str!("shaders/warp.wgsl");

/// Largest box radius the tiled kernels support.
pub const BOX_RADIUS_MAX: u32 = 8;

/// Uniform of `k_axpy`.
#[cfg(feature = "dev-tools")]
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct AxpyParams {
    pub n: u32,
    pub a: f32,
    pub pad: [u32; 2],
}

/// Uniform shared by the CC kernels.
#[cfg(feature = "dev-tools")]
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GridParams {
    pub nx: u32,
    pub ny: u32,
    pub nz: u32,
    pub n: u32,
    pub shift_i: f32,
    pub shift_j: f32,
    pub pad: [u32; 2],
}

/// Uniform of the box-sum kernels.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct BoxParams {
    pub nx: u32,
    pub ny: u32,
    pub nz: u32,
    pub n: u32,
    pub axis_len: u32,
    pub axis_stride: u32,
    pub other_stride: u32,
    pub radius: u32,
    pub planes: u32,
    pub pad: [u32; 3],
}

/// Uniform of `k_reduce_final`.
#[cfg(feature = "dev-tools")]
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct ReduceParams {
    pub count: u32,
    pub pad: [u32; 3],
}

/// Uniform of `k_warp_scalar`; rows `m0..m2` hold the 3x4 affine (matrix row, translation).
#[cfg(feature = "dev-tools")]
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct WarpParams {
    pub nx: u32,
    pub ny: u32,
    pub nz: u32,
    pub n: u32,
    pub snx: u32,
    pub sny: u32,
    pub snz: u32,
    pub flags: u32,
    pub vo: [f32; 4],
    pub vs: [f32; 4],
    pub so: [f32; 4],
    pub ss: [f32; 4],
    pub m0: [f32; 4],
    pub m1: [f32; 4],
    pub m2: [f32; 4],
}

/// The compiled probe pipelines.
#[cfg(feature = "dev-tools")]
pub struct Kernels {
    pub axpy: wgpu::ComputePipeline,
    pub noop: wgpu::ComputePipeline,
    pub cc_intermediates: wgpu::ComputePipeline,
    pub cc_derivative: wgpu::ComputePipeline,
    pub reduce_final: wgpu::ComputePipeline,
    pub box_x: wgpu::ComputePipeline,
    pub box_yz: wgpu::ComputePipeline,
    pub warp: wgpu::ComputePipeline,
}

#[cfg(feature = "dev-tools")]
impl Kernels {
    /// Compiles every probe kernel; shader or pipeline errors are returned verbatim.
    pub fn new(ctx: &GpuContext) -> GpuResult<Self> {
        Ok(Kernels {
            axpy: ctx.compile("k_axpy", SRC_AXPY, "k_axpy")?,
            noop: ctx.compile("k_noop", SRC_NOOP, "k_noop")?,
            cc_intermediates: ctx.compile("k_cc_intermediates", SRC_CC, "k_cc_intermediates")?,
            cc_derivative: ctx.compile(
                "k_cc_value_derivative",
                SRC_CC_DERIV,
                "k_cc_value_derivative",
            )?,
            reduce_final: ctx.compile("k_reduce_final", SRC_REDUCE, "k_reduce_final")?,
            box_x: ctx.compile("k_box_x", SRC_BOX, "k_box_x")?,
            box_yz: ctx.compile("k_box_yz", SRC_BOX, "k_box_yz")?,
            warp: ctx.compile("k_warp_scalar", SRC_WARP, "k_warp_scalar")?,
        })
    }
}

impl GpuContext {
    /// Compiles one WGSL entry point into a compute pipeline with an automatic layout.
    pub fn compile(
        &self,
        label: &str,
        source: &str,
        entry: &str,
    ) -> GpuResult<wgpu::ComputePipeline> {
        self.scoped(label, 0, || {
            let module = self
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(label),
                    source: wgpu::ShaderSource::Wgsl(Cow::Owned(source.to_string())),
                });
            self.device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(label),
                    layout: None,
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        zero_initialize_workgroup_memory: false,
                        ..Default::default()
                    },
                    cache: None,
                })
        })
    }

    /// Binds `buffers` to bindings `0..` of group 0 of `pipeline`; each binds its whole buffer.
    pub fn bind(
        &self,
        pipeline: &wgpu::ComputePipeline,
        buffers: &[&wgpu::Buffer],
    ) -> GpuResult<wgpu::BindGroup> {
        self.scoped("bind group", 0, || {
            let layout = pipeline.get_bind_group_layout(0);
            let entries: Vec<wgpu::BindGroupEntry> = buffers
                .iter()
                .enumerate()
                .map(|(i, b)| wgpu::BindGroupEntry {
                    binding: i as u32,
                    resource: b.as_entire_binding(),
                })
                .collect();
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &layout,
                entries: &entries,
            })
        })
    }

    /// Creates a uniform buffer holding `value`.
    pub fn uniform<T: Pod>(&self, label: &str, value: &T) -> GpuResult<wgpu::Buffer> {
        let bytes = bytemuck::bytes_of(value);
        let buffer = self.scoped(label, bytes.len() as u64, || {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        })?;
        self.queue.write_buffer(&buffer, 0, bytes);
        Ok(buffer)
    }

    /// Uploads `data` to the start of `buffer`.
    pub fn upload<T: Pod>(&self, buffer: &wgpu::Buffer, data: &[T]) {
        self.queue
            .write_buffer(buffer, 0, bytemuck::cast_slice(data));
    }
}

/// Workgroup count along one dimension.
pub fn groups(n: u32, per_group: u32) -> u32 {
    n.div_ceil(per_group)
}
