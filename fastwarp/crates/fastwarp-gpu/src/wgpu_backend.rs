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
// Portions derived from ITK `itkSyNImageRegistrationMethod.hxx`,
// `itkANTSNeighborhoodCorrelationImageToImageMetricv4GetValueAndDerivativeThreader.hxx` and
// `itkImageRegistrationMethodv4.hxx` (the host orchestration of the registration kernels). See
// `docs/THIRD_PARTY_PORTS.md`.

//! `WgpuBackend`: the `f32` GPU implementation of `fastwarp_reg::Backend`.
//!
//! Images and fields stay resident in pooled storage buffers (fields are structure-of-arrays:
//! `x` plane, `y` plane, `z` plane). Host round trips are limited to the scalars the SyN loop
//! consumes (see [`BackendStats`]); every kernel is a gather and every reduction a fixed-order
//! tree, so results are bit-identical from run to run on one device.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytemuck::Pod;
use fastwarp_reg::backend::{Backend, InversionPair, MetricEvaluation, MetricPair};
use fastwarp_reg::error::{RegError, Result as RegResult};
use fastwarp_reg::field_ops::{
    gaussian_operator_coefficients, InvertParams, InvertStats, VectorField,
};
use fastwarp_reg::grid::Grid;
use fastwarp_reg::linalg::{mat3_inverse, mat3_mul, mat3_mul_vec, Mat3, Vec3, IDENTITY3};
use fastwarp_reg::transform::AffineTransform;
use fastwarp_reg::volume::Volume;

use crate::device::{BackendSelect, GpuContext};
use crate::error::{GpuError, GpuResult};
use crate::kernels::{BoxParams, BOX_RADIUS_MAX};
use crate::parity;
use crate::pool::BufferPool;
mod mi;

pub use mi::GpuMiLevel;

use crate::syn_kernels::{
    CcFinalU, CcPlanesU, ComposeU, ElementU, GaussU, InvBeginU, InvReduceU, InvResidualU,
    InvUpdateU, NormU, ReduceU, SynKernels, WarpU,
};

const SLOT_BYTES: u64 = 256;
const RING_SLOTS: u64 = 8192;
const GAUSS_MAX_ERROR: f64 = 0.001;
const SELFTEST_ATTEMPTS: u32 = 3;
const INVERT_BODIES_PER_SUBMIT: u32 = 1024;
const SCALAR_STAGING_BYTES: u64 = 256;
/// Storage buffers `k_cc_deriv` binds: the five split window-sum planes, the fixed and moving
/// images and the derivative field.
const CC_DERIV_STORAGE_BUFFERS: u32 = 8;

fn bad(message: impl Into<String>) -> RegError {
    RegError::Invalid {
        what: "wgpu backend",
        message: message.into(),
    }
}

fn gerr(e: GpuError) -> RegError {
    bad(e.to_string())
}

struct PoolBuf {
    buf: wgpu::Buffer,
    pool: Arc<Mutex<BufferPool>>,
}

impl PoolBuf {
    fn buffer(&self) -> &wgpu::Buffer {
        &self.buf
    }
}

impl Drop for PoolBuf {
    fn drop(&mut self) {
        let mut pool = self.pool.lock().unwrap_or_else(|p| p.into_inner());
        pool.release(self.buf.clone());
    }
}

/// A scalar volume resident on the device (`f32`, x-fastest).
pub struct GpuImage {
    buf: PoolBuf,
    grid: Grid,
}

impl GpuImage {
    /// The grid of the image.
    pub fn grid(&self) -> &Grid {
        &self.grid
    }
}

/// A displacement field resident on the device (`f32`, three planes of `n` values).
pub struct GpuField {
    buf: PoolBuf,
    grid: Grid,
}

struct TapSet {
    buf: wgpu::Buffer,
    offset: [u32; 3],
    radius: [u32; 3],
}

type TapKey = (u64, [usize; 3]);

struct Ring {
    buf: wgpu::Buffer,
    next: AtomicU64,
    stride: u64,
}

/// Counters of host interaction, for documentation and benchmarks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackendStats {
    /// Scalar read-backs (each is one submit, one map, one wait).
    pub scalar_readbacks: u64,
    /// Whole-field downloads.
    pub field_downloads: u64,
    /// Whole-image or whole-field uploads.
    pub uploads: u64,
    /// Command buffers submitted.
    pub submits: u64,
}

/// GPU time of one operation kind.
#[derive(Debug, Clone, PartialEq)]
pub struct OpProfile {
    /// Operation label.
    pub op: String,
    /// Number of calls.
    pub calls: u64,
    /// GPU seconds summed over all calls.
    pub seconds: f64,
}

/// Result of a profiled run.
#[derive(Debug, Clone, Default)]
pub struct ProfileReport {
    /// Per-operation totals, sorted by descending time.
    pub ops: Vec<OpProfile>,
    /// Every operation in call order with its GPU seconds.
    pub timeline: Vec<(String, f64)>,
}

#[derive(Default)]
struct Profile {
    enabled: bool,
    ops: HashMap<&'static str, (u64, f64)>,
    timeline: Vec<(&'static str, f64)>,
}

/// Record of the device-creation known-answer check.
#[derive(Debug, Clone, Default)]
pub struct SelfTestRecord {
    /// Number of attempts made overall (more than 1 means an earlier attempt mismatched and the
    /// kernels were recompiled before the next one). The worst of the two families.
    pub attempts: u32,
    /// Attempts the SyN kernel family needed.
    pub syn_attempts: u32,
    /// Attempts the Mattes MI kernel family needed.
    pub mi_attempts: u32,
    /// Result lines of the passing attempt.
    pub lines: Vec<String>,
    /// Mismatch lines of the failed first attempt, when a retry was needed.
    pub first_attempt_failures: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct FamilyCheck {
    attempts: u32,
    lines: Vec<String>,
    first_failures: Vec<String>,
}

impl FamilyCheck {
    fn passed(attempts: u32, lines: Vec<String>, first_failures: Vec<String>) -> Self {
        Self {
            attempts,
            lines,
            first_failures,
        }
    }
}

/// The wgpu implementation of the registration backend.
pub struct WgpuBackend {
    ctx: Arc<GpuContext>,
    kernels: SynKernels,
    mi_kernels: mi::MiKernels,
    pool: Arc<Mutex<BufferPool>>,
    ring: Ring,
    taps: Mutex<HashMap<TapKey, Arc<TapSet>>>,
    profile: Mutex<Profile>,
    max_groups: u32,
    scalar_readbacks: AtomicU64,
    field_downloads: AtomicU64,
    uploads: AtomicU64,
    submits: AtomicU64,
    selftest: Mutex<SelfTestRecord>,
    scalar_staging: Mutex<wgpu::Buffer>,
    partial_staging: Mutex<Option<wgpu::Buffer>>,
    trimmed_for: Mutex<Option<Grid>>,
}

fn dir_scaled(g: &Grid) -> Mat3 {
    let d = g.direction();
    let s = g.spacing();
    let mut out = [[0.0; 3]; 3];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell = d[r][c] * s[c];
        }
    }
    out
}

fn index_from_offset(g: &Grid) -> RegResult<Mat3> {
    mat3_inverse(&dir_scaled(g)).ok_or_else(|| bad("singular grid direction"))
}

fn row4(m: &Mat3, r: usize, w: f64) -> [f32; 4] {
    [m[r][0] as f32, m[r][1] as f32, m[r][2] as f32, w as f32]
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn dims4(g: &Grid) -> [u32; 4] {
    let s = g.size();
    [s[0] as u32, s[1] as u32, s[2] as u32, g.num_voxels() as u32]
}

fn spacing4(g: &Grid) -> [f32; 4] {
    let s = g.spacing();
    [s[0] as f32, s[1] as f32, s[2] as f32, 0.0]
}

/// Builds the uniform that maps destination voxel indices to source continuous indices:
/// `c = Mi * idx + c0 (+ L * u)`, with the field added before the affine as ITK composes it.
fn warp_uniform(
    dst: &Grid,
    src: &Grid,
    affine: Option<&AffineTransform>,
    has_field: bool,
    nplanes: u32,
) -> RegResult<WarpU> {
    let ifo = index_from_offset(src)?;
    let (mx, off) = match affine {
        Some(a) => (a.matrix, a.offset()),
        None => (IDENTITY3, [0.0; 3]),
    };
    let a = mat3_mul(&ifo, &mx);
    let mi = mat3_mul(&a, &dir_scaled(dst));
    let c0 = add(
        mat3_mul_vec(&a, dst.origin()),
        mat3_mul_vec(&ifo, sub(off, src.origin())),
    );
    let sd = dims4(src);
    let dd = dims4(dst);
    Ok(WarpU {
        dims: dd,
        src_dims: sd,
        flags: u32::from(has_field),
        nplanes,
        pad: [0; 2],
        mi: [
            row4(&mi, 0, c0[0]),
            row4(&mi, 1, c0[1]),
            row4(&mi, 2, c0[2]),
        ],
        l: [row4(&a, 0, 0.0), row4(&a, 1, 0.0), row4(&a, 2, 0.0)],
    })
}

impl WgpuBackend {
    /// Creates the device (Vulkan preferred, `FASTWARP_GPU_BACKEND` honoured), compiles the kernels
    /// and runs the known-answer check of the SyN and Mattes MI kernel families. A mismatch on a
    /// cold shader cache (seen on DX12) recompiles that family and retries (up to `SELFTEST_ATTEMPTS` attempts); a mismatch that
    /// persists fails with a clear error.
    pub fn new(select: &BackendSelect) -> GpuResult<Self> {
        GpuContext::new_with(select, Self::from_context)
    }

    /// Wraps an existing context.
    pub fn from_context(ctx: GpuContext) -> GpuResult<Self> {
        Self::from_shared_context(Arc::new(ctx))
    }

    /// Wraps a context that other engines (N4) share, so a run creates the device once.
    pub fn from_shared_context(ctx: Arc<GpuContext>) -> GpuResult<Self> {
        let kernels = SynKernels::new(&ctx)?;
        let mi_kernels = mi::MiKernels::new(&ctx)?;
        let alignment = u64::from(ctx.device.limits().min_uniform_buffer_offset_alignment);
        let stride = SLOT_BYTES.max(alignment);
        let ring_buf = ctx.scoped("uniform ring", stride * RING_SLOTS, || {
            ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fastwarp-gpu uniform ring"),
                size: stride * RING_SLOTS,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        })?;
        let max_groups = ctx.device.limits().max_compute_workgroups_per_dimension;
        let scalar_staging = ctx.staging_buffer(SCALAR_STAGING_BYTES)?;
        let mut backend = WgpuBackend {
            ctx,
            kernels,
            mi_kernels,
            pool: Arc::new(Mutex::new(BufferPool::new())),
            ring: Ring {
                buf: ring_buf,
                next: AtomicU64::new(0),
                stride,
            },
            taps: Mutex::new(HashMap::new()),
            profile: Mutex::new(Profile::default()),
            max_groups,
            scalar_readbacks: AtomicU64::new(0),
            field_downloads: AtomicU64::new(0),
            uploads: AtomicU64::new(0),
            submits: AtomicU64::new(0),
            selftest: Mutex::new(SelfTestRecord::default()),
            scalar_staging: Mutex::new(scalar_staging),
            partial_staging: Mutex::new(None),
            trimmed_for: Mutex::new(None),
        };
        backend.run_selftest()?;
        Ok(backend)
    }

    fn run_selftest(&mut self) -> GpuResult<()> {
        let syn = self.selftest_syn(&parity::Case::selftest())?;
        let mi = self.selftest_mi(&parity::Case::selftest_mi())?;
        let mut rec = self.selftest.lock().unwrap_or_else(|p| p.into_inner());
        *rec = SelfTestRecord {
            attempts: syn.attempts.max(mi.attempts),
            syn_attempts: syn.attempts,
            mi_attempts: mi.attempts,
            lines: [syn.lines, mi.lines].concat(),
            first_attempt_failures: [syn.first_failures, mi.first_failures].concat(),
        };
        Ok(())
    }

    fn selftest_syn(&mut self, case: &parity::Case) -> GpuResult<FamilyCheck> {
        let mut first = Vec::new();
        for attempt in 1..=SELFTEST_ATTEMPTS {
            let results = parity::check_all(self, case)
                .map_err(|e| GpuError::Verification(format!("known-answer SyN run failed: {e}")))?;
            let failures: Vec<String> =
                results.iter().filter(|r| !r.ok).map(|r| r.line()).collect();
            if failures.is_empty() {
                return Ok(FamilyCheck::passed(
                    attempt,
                    results.iter().map(|r| r.line()).collect(),
                    first,
                ));
            }
            if attempt == 1 {
                first = failures.clone();
            }
            if attempt == SELFTEST_ATTEMPTS {
                return Err(self.selftest_failed("SyN", &failures));
            }
            self.kernels = SynKernels::new(&self.ctx)?;
        }
        Ok(FamilyCheck::default())
    }

    fn selftest_mi(&mut self, case: &parity::Case) -> GpuResult<FamilyCheck> {
        let mut first = Vec::new();
        for attempt in 1..=SELFTEST_ATTEMPTS {
            let mut results = parity::check_mi(self, case)
                .map_err(|e| GpuError::Verification(format!("known-answer MI run failed: {e}")))?;
            results.extend(parity::check_mi_chunked(self, case).map_err(|e| {
                GpuError::Verification(format!("known-answer chunked MI run failed: {e}"))
            })?);
            let failures: Vec<String> =
                results.iter().filter(|r| !r.ok).map(|r| r.line()).collect();
            if failures.is_empty() {
                return Ok(FamilyCheck::passed(
                    attempt,
                    results.iter().map(|r| r.line()).collect(),
                    first,
                ));
            }
            if attempt == 1 {
                first = failures.clone();
            }
            if attempt == SELFTEST_ATTEMPTS {
                return Err(self.selftest_failed("MI", &failures));
            }
            self.mi_kernels = mi::MiKernels::new(&self.ctx)?;
        }
        Ok(FamilyCheck::default())
    }

    fn selftest_failed(&self, family: &str, failures: &[String]) -> GpuError {
        GpuError::Verification(format!(
            "known-answer check of the {family} kernels failed on {} / {} in {} attempts with \
             freshly compiled kernels: {}",
            self.ctx.info.name,
            self.ctx.info.backend,
            SELFTEST_ATTEMPTS,
            failures.join("; ")
        ))
    }

    /// The device context.
    pub fn context(&self) -> &GpuContext {
        &self.ctx
    }

    /// A shared handle to the device context, for engines that run on the same device.
    pub fn shared_context(&self) -> Arc<GpuContext> {
        Arc::clone(&self.ctx)
    }

    /// The record of the device-creation known-answer check.
    pub fn selftest_record(&self) -> SelfTestRecord {
        self.selftest
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Host-interaction counters.
    pub fn stats(&self) -> BackendStats {
        BackendStats {
            scalar_readbacks: self.scalar_readbacks.load(Ordering::Relaxed),
            field_downloads: self.field_downloads.load(Ordering::Relaxed),
            uploads: self.uploads.load(Ordering::Relaxed),
            submits: self.submits.load(Ordering::Relaxed),
        }
    }

    /// Bytes the buffer pool currently owns on the device.
    pub fn allocated_bytes(&self) -> u64 {
        self.pool
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .allocated_bytes()
    }

    /// Largest number of bytes the buffer pool has owned at once.
    pub fn peak_allocated_bytes(&self) -> u64 {
        self.pool
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .peak_bytes()
    }

    /// Frees every idle pooled buffer and the cached Gaussian taps, returning the bytes freed.
    /// Buffers still held by live images and fields are untouched and return to the pool when
    /// dropped.
    pub fn release_idle(&self) -> u64 {
        self.taps.lock().unwrap_or_else(|p| p.into_inner()).clear();
        self.forget_trim();
        self.pool
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .release_idle()
    }

    fn trim_pool_for(&self, grid: &Grid) {
        let n = grid.num_voxels() as u64;
        self.pool
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain_sizes(&[n * 4, n * 12]);
    }

    fn trim_pool_once_for(&self, grid: &Grid) {
        let mut trimmed = self.trimmed_for.lock().unwrap_or_else(|p| p.into_inner());
        if trimmed.as_ref() != Some(grid) {
            self.trim_pool_for(grid);
            *trimmed = Some(*grid);
        }
    }

    fn forget_trim(&self) {
        *self.trimmed_for.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Turns per-operation GPU timing on or off. While on, every operation is submitted alone and
    /// waited for (timestamps bracket its commands), so wall-clock time grows.
    pub fn set_profiling(&self, enabled: bool) {
        let mut p = self.profile.lock().unwrap_or_else(|p| p.into_inner());
        p.enabled = enabled;
    }

    /// Returns and clears the profile collected since the last call.
    pub fn take_profile(&self) -> ProfileReport {
        let mut p = self.profile.lock().unwrap_or_else(|p| p.into_inner());
        let mut ops: Vec<OpProfile> = p
            .ops
            .iter()
            .map(|(op, (calls, seconds))| OpProfile {
                op: (*op).to_string(),
                calls: *calls,
                seconds: *seconds,
            })
            .collect();
        ops.sort_by(|a, b| b.seconds.total_cmp(&a.seconds));
        let timeline = p
            .timeline
            .iter()
            .map(|(op, s)| ((*op).to_string(), *s))
            .collect();
        p.ops.clear();
        p.timeline.clear();
        ProfileReport { ops, timeline }
    }

    fn acquire(&self, label: &str, bytes: u64) -> RegResult<PoolBuf> {
        let limit = self.ctx.caps.max_storage_buffer_binding_size;
        if bytes > limit {
            return Err(bad(format!(
                "{label} needs {bytes} bytes, the device binds at most {limit} bytes per storage buffer"
            )));
        }
        let buf = self
            .pool
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .acquire(&self.ctx, label, bytes)
            .map_err(gerr)?;
        Ok(PoolBuf {
            buf,
            pool: self.pool.clone(),
        })
    }

    fn image_buffer(&self, grid: &Grid) -> RegResult<PoolBuf> {
        self.acquire("image", grid.num_voxels() as u64 * 4)
    }

    fn field_buffer(&self, grid: &Grid) -> RegResult<PoolBuf> {
        self.acquire("field", grid.num_voxels() as u64 * 12)
    }

    fn run_encoder(
        &self,
        label: &'static str,
        record: impl FnOnce(&mut wgpu::CommandEncoder) -> RegResult<()>,
    ) -> RegResult<()> {
        let profiling = self
            .profile
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .enabled;
        self.submits.fetch_add(1, Ordering::Relaxed);
        let first_slot = self.ring.next.load(Ordering::Relaxed);
        if profiling {
            let mut result = Ok(());
            let timing = self
                .ctx
                .timed(false, |enc| {
                    result = record(enc).and_then(|()| self.check_ring(label, first_slot))
                })
                .map_err(gerr)?;
            result?;
            let mut p = self.profile.lock().unwrap_or_else(|p| p.into_inner());
            let entry = p.ops.entry(label).or_insert((0, 0.0));
            entry.0 += 1;
            entry.1 += timing.seconds;
            p.timeline.push((label, timing.seconds));
        } else {
            let mut enc = self
                .ctx
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
            record(&mut enc)?;
            self.check_ring(label, first_slot)?;
            self.ctx.queue.submit(Some(enc.finish()));
        }
        self.ctx
            .check_errors()
            .map_err(|e| bad(format!("{label}: {e}")))
    }

    fn check_ring(&self, label: &str, first_slot: u64) -> RegResult<()> {
        let used = self.ring.next.load(Ordering::Relaxed) - first_slot;
        if used > RING_SLOTS {
            return Err(bad(format!(
                "{label} recorded {used} launches in one submission, more than the {RING_SLOTS} \
                 slots of the uniform ring"
            )));
        }
        Ok(())
    }

    fn run_pass(
        &self,
        label: &'static str,
        record: impl FnOnce(&mut wgpu::ComputePass<'_>) -> RegResult<()>,
    ) -> RegResult<()> {
        self.run_encoder(label, |enc| {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(label),
                timestamp_writes: None,
            });
            record(&mut pass)
        })
    }

    fn launch<U: Pod>(
        &self,
        pass: &mut wgpu::ComputePass<'_>,
        pipeline: &wgpu::ComputePipeline,
        buffers: &[&wgpu::Buffer],
        uniform: &U,
        groups: [u32; 3],
    ) -> RegResult<()> {
        let group = self.prepare(pipeline, buffers, uniform)?;
        self.dispatch(pass, pipeline, &group, groups)
    }

    fn prepare<U: Pod>(
        &self,
        pipeline: &wgpu::ComputePipeline,
        buffers: &[&wgpu::Buffer],
        uniform: &U,
    ) -> RegResult<wgpu::BindGroup> {
        let slot = self.ring.next.fetch_add(1, Ordering::Relaxed) % RING_SLOTS;
        let offset = slot * self.ring.stride;
        let bytes = bytemuck::bytes_of(uniform);
        self.ctx.queue.write_buffer(&self.ring.buf, offset, bytes);
        let size = NonZeroU64::new(bytes.len() as u64).ok_or_else(|| bad("empty uniform"))?;
        let layout = pipeline.get_bind_group_layout(0);
        let mut entries: Vec<wgpu::BindGroupEntry<'_>> = buffers
            .iter()
            .enumerate()
            .map(|(i, b)| wgpu::BindGroupEntry {
                binding: i as u32,
                resource: b.as_entire_binding(),
            })
            .collect();
        entries.push(wgpu::BindGroupEntry {
            binding: buffers.len() as u32,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &self.ring.buf,
                offset,
                size: Some(size),
            }),
        });
        Ok(self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &layout,
                entries: &entries,
            }))
    }

    fn dispatch(
        &self,
        pass: &mut wgpu::ComputePass<'_>,
        pipeline: &wgpu::ComputePipeline,
        group: &wgpu::BindGroup,
        groups: [u32; 3],
    ) -> RegResult<()> {
        let max = self.max_groups;
        if groups.iter().any(|g| *g > max) {
            return Err(bad(format!(
                "dispatch {groups:?} exceeds max_compute_workgroups_per_dimension {max}"
            )));
        }
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, group, &[]);
        pass.dispatch_workgroups(groups[0], groups[1], groups[2]);
        Ok(())
    }

    /// Copies an image back to the host as `f64`.
    pub fn download_image(&self, image: &GpuImage) -> RegResult<Vec<f64>> {
        let n = image.grid.num_voxels();
        let data = self.ctx.read_f32(image.buf.buffer(), n).map_err(gerr)?;
        Ok(data.into_iter().map(f64::from).collect())
    }

    fn groups_3d(&self, g: &Grid) -> [u32; 3] {
        let s = g.size();
        [
            (s[0] as u32).div_ceil(8),
            (s[1] as u32).div_ceil(8),
            (s[2] as u32).div_ceil(4),
        ]
    }

    fn groups_1d(&self, items: u64) -> RegResult<[u32; 3]> {
        let g = items.div_ceil(256).max(1);
        let max = u64::from(self.max_groups);
        let gx = g.min(max).min(32768);
        let gy = g.div_ceil(gx);
        if gy > max {
            return Err(bad(format!("{items} items exceed the dispatch limits")));
        }
        Ok([gx as u32, gy as u32, 1])
    }

    fn read_scalar_bytes(&self, buffer: &wgpu::Buffer, len: u64) -> RegResult<Vec<u8>> {
        self.scalar_readbacks.fetch_add(1, Ordering::Relaxed);
        if len > SCALAR_STAGING_BYTES {
            return self.ctx.read_bytes(buffer, 0, len).map_err(gerr);
        }
        let staging = self
            .scalar_staging
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        self.ctx
            .read_bytes_staged(buffer, 0, len, &staging)
            .map_err(gerr)
    }

    fn read_scalars(&self, buffer: &wgpu::Buffer, count: usize) -> RegResult<Vec<f32>> {
        let bytes = self.read_scalar_bytes(buffer, (count * 4) as u64)?;
        Ok(bytemuck::pod_collect_to_vec(&bytes))
    }

    fn read_sources(&self, sources: &[(&wgpu::Buffer, u64)]) -> RegResult<Vec<u8>> {
        let len: u64 = sources.iter().map(|(_, bytes)| bytes).sum();
        self.scalar_readbacks.fetch_add(1, Ordering::Relaxed);
        if len <= SCALAR_STAGING_BYTES {
            let staging = self
                .scalar_staging
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            return self.read_sources_staged(sources, len, &staging);
        }
        let mut staging = self
            .partial_staging
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let buf = match staging.take() {
            Some(b) if b.size() >= len => b,
            _ => self.ctx.staging_buffer(len).map_err(gerr)?,
        };
        let values = self.read_sources_staged(sources, len, &buf);
        *staging = Some(buf);
        values
    }

    fn read_sources_staged(
        &self,
        sources: &[(&wgpu::Buffer, u64)],
        len: u64,
        staging: &wgpu::Buffer,
    ) -> RegResult<Vec<u8>> {
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fastwarp-gpu readback"),
            });
        let mut offset = 0;
        for (buffer, bytes) in sources {
            enc.copy_buffer_to_buffer(buffer, 0, staging, offset, *bytes);
            offset += bytes;
        }
        self.ctx.queue.submit(Some(enc.finish()));
        self.ctx.read_staging(staging, len).map_err(gerr)
    }

    fn require_same_grid(&self, what: &str, a: &Grid, b: &Grid) -> RegResult<()> {
        if a != b {
            return Err(bad(format!("{what}: grids differ")));
        }
        Ok(())
    }

    fn tap_set(&self, variance: f64, size: [usize; 3]) -> RegResult<Arc<TapSet>> {
        let key = (variance.to_bits(), size);
        let mut cache = self.taps.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = cache.get(&key) {
            return Ok(t.clone());
        }
        let mut all: Vec<f32> = Vec::new();
        let mut offset = [0u32; 3];
        let mut radius = [0u32; 3];
        for axis in 0..3 {
            let taps = gaussian_operator_coefficients(variance, GAUSS_MAX_ERROR, size[axis]);
            offset[axis] = all.len() as u32;
            radius[axis] = (taps.len() / 2) as u32;
            all.extend(taps.iter().map(|t| *t as f32));
        }
        let bytes = (all.len() * 4) as u64;
        let buf = self
            .ctx
            .scoped("gaussian taps", bytes, || {
                self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("gaussian taps"),
                    size: bytes,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .map_err(gerr)?;
        self.ctx
            .queue
            .write_buffer(&buf, 0, bytemuck::cast_slice(&all));
        let set = Arc::new(TapSet {
            buf,
            offset,
            radius,
        });
        if cache.len() > 64 {
            cache.clear();
        }
        cache.insert(key, set.clone());
        Ok(set)
    }

    fn reduction_partials(&self, grid: &Grid) -> RegResult<PoolBuf> {
        let groups = self.groups_1d(grid.num_voxels() as u64)?;
        self.acquire("reduction partials", u64::from(groups[0] * groups[1]) * 4)
    }

    fn encode_reduce(
        &self,
        pass: &mut wgpu::ComputePass<'_>,
        field: &wgpu::Buffer,
        grid: &Grid,
        mode: u32,
        part: &wgpu::Buffer,
        scalar: &wgpu::Buffer,
    ) -> RegResult<()> {
        let n = grid.num_voxels();
        let groups = self.groups_1d(n as u64)?;
        let count = groups[0] * groups[1];
        self.launch(
            pass,
            &self.kernels.norm_partial,
            &[field, part],
            &NormU {
                n: n as u32,
                mode,
                pad: [0; 2],
                sp: spacing4(grid),
            },
            groups,
        )?;
        self.launch(
            pass,
            &self.kernels.reduce_scalar,
            &[part, scalar],
            &ReduceU {
                count,
                mode,
                out_index: 0,
                pad: 0,
            },
            [1, 1, 1],
        )
    }

    fn reduce_field(&self, field: &GpuField, mode: u32, label: &'static str) -> RegResult<f32> {
        let part = self.reduction_partials(&field.grid)?;
        let scalar = self.acquire("reduction scalar", 16)?;
        self.run_pass(label, |pass| {
            self.encode_reduce(
                pass,
                field.buf.buffer(),
                &field.grid,
                mode,
                part.buffer(),
                scalar.buffer(),
            )
        })?;
        let v = self.read_scalars(scalar.buffer(), 1)?;
        Ok(v[0])
    }

    /// The CC metric of `(fixed, moving)` and, when `swapped`, of `(moving, fixed)` from the same
    /// window sums: the five sums are symmetric under the swap (`a <-> b`, `aa <-> bb`, `ab`), so
    /// the second direction only rebinds them. Results are in evaluation order; with `norms`, the
    /// second vector holds the [`Backend::l2_norm`] of each derivative, read back together with
    /// the metric values.
    fn cc_evaluate(
        &self,
        fixed: &GpuImage,
        moving: &GpuImage,
        radius: usize,
        swapped: bool,
        norms: bool,
    ) -> RegResult<(Vec<MetricEvaluation<GpuField>>, Vec<f64>)> {
        self.require_same_grid("cc", &fixed.grid, &moving.grid)?;
        if radius == 0 || radius > BOX_RADIUS_MAX as usize {
            return Err(bad(format!(
                "cc radius {radius} outside 1..={BOX_RADIUS_MAX}"
            )));
        }
        if self.ctx.caps.max_storage_buffers_per_shader_stage < CC_DERIV_STORAGE_BUFFERS {
            return Err(bad(format!(
                "the CC derivative kernel binds {CC_DERIV_STORAGE_BUFFERS} storage buffers, the \
                 device allows {}",
                self.ctx.caps.max_storage_buffers_per_shader_stage
            )));
        }
        let grid = fixed.grid;
        let n = grid.num_voxels();
        let [nx, ny, nz] = grid.size();
        let plane_bytes = n as u64 * 4;
        let planes = [
            self.acquire("cc plane a", plane_bytes)?,
            self.acquire("cc plane b", plane_bytes)?,
            self.acquire("cc plane aa", plane_bytes)?,
            self.acquire("cc plane bb", plane_bytes)?,
            self.acquire("cc plane ab", plane_bytes)?,
        ];
        let sums = [
            self.acquire("cc sum a", plane_bytes)?,
            self.acquire("cc sum b", plane_bytes)?,
            self.acquire("cc sum aa", plane_bytes)?,
            self.acquire("cc sum bb", plane_bytes)?,
            self.acquire("cc sum ab", plane_bytes)?,
        ];
        let directions = if swapped { 2 } else { 1 };
        let g3 = self.groups_3d(&grid);
        let count = g3[0] * g3[1] * g3[2];
        let mut outs = Vec::with_capacity(directions);
        for _ in 0..directions {
            outs.push(self.field_buffer(&grid)?);
        }
        let parts_bytes = u64::from(count) * directions as u64 * 4;
        let parts = self.acquire("cc partials", parts_bytes)?;
        let reduction = if norms {
            let part = self.reduction_partials(&grid)?;
            let mut scalars = Vec::with_capacity(directions);
            for _ in 0..directions {
                scalars.push(self.acquire("reduction scalar", 16)?);
            }
            Some((part, scalars))
        } else {
            None
        };
        let ifo = index_from_offset(&grid)?;
        let spacing = grid.spacing();
        let mut offsets = [[0.0f32; 4]; 3];
        for d in 0..3 {
            for r in 0..3 {
                offsets[d][r] = (0.5 * spacing[d] * ifo[r][d]) as f32;
            }
        }
        let r = radius as u32;
        let uniform = CcFinalU {
            dims: [nx as u32, ny as u32, nz as u32, n as u32],
            radius: r,
            part_base: 0,
            pad: [0; 2],
            o: offsets,
            sp: spacing4(&grid),
        };
        let label = if swapped {
            "cc_value_derivative_pair"
        } else {
            "cc_value_derivative"
        };
        self.run_pass(label, |pass| {
            self.launch(
                pass,
                &self.kernels.cc_planes,
                &[
                    fixed.buf.buffer(),
                    moving.buf.buffer(),
                    planes[0].buffer(),
                    planes[1].buffer(),
                    planes[2].buffer(),
                    planes[3].buffer(),
                    planes[4].buffer(),
                ],
                &CcPlanesU {
                    n: n as u32,
                    pad: [0; 3],
                },
                self.groups_1d(n as u64)?,
            )?;
            for p in 0..5 {
                self.box_pass(pass, planes[p].buffer(), sums[p].buffer(), &grid, 0, r)?;
            }
            for p in 0..5 {
                self.box_pass(pass, sums[p].buffer(), planes[p].buffer(), &grid, 1, r)?;
            }
            for p in 0..5 {
                self.box_pass(pass, planes[p].buffer(), sums[p].buffer(), &grid, 2, r)?;
            }
            for (d, out) in outs.iter().enumerate() {
                let order = if d == 0 {
                    [0, 1, 2, 3, 4]
                } else {
                    [1, 0, 3, 2, 4]
                };
                let (src_fixed, src_moving) = if d == 0 {
                    (fixed.buf.buffer(), moving.buf.buffer())
                } else {
                    (moving.buf.buffer(), fixed.buf.buffer())
                };
                self.launch(
                    pass,
                    &self.kernels.cc_deriv,
                    &[
                        sums[order[0]].buffer(),
                        sums[order[1]].buffer(),
                        sums[order[2]].buffer(),
                        sums[order[3]].buffer(),
                        sums[order[4]].buffer(),
                        src_fixed,
                        src_moving,
                        out.buffer(),
                    ],
                    &uniform,
                    g3,
                )?;
                self.launch(
                    pass,
                    &self.kernels.cc_value,
                    &[
                        sums[order[0]].buffer(),
                        sums[order[1]].buffer(),
                        sums[order[2]].buffer(),
                        sums[order[3]].buffer(),
                        sums[order[4]].buffer(),
                        parts.buffer(),
                    ],
                    &CcFinalU {
                        part_base: d as u32 * count,
                        ..uniform
                    },
                    g3,
                )?;
            }
            if let Some((part, scalars)) = &reduction {
                for (out, scalar) in outs.iter().zip(scalars) {
                    self.encode_reduce(
                        pass,
                        out.buffer(),
                        &grid,
                        1,
                        part.buffer(),
                        scalar.buffer(),
                    )?;
                }
            }
            Ok(())
        })?;
        let mut sources = vec![(parts.buffer(), parts_bytes)];
        if let Some((_, scalars)) = &reduction {
            sources.extend(scalars.iter().map(|s| (s.buffer(), 4)));
        }
        let values: Vec<f32> = bytemuck::pod_collect_to_vec(&self.read_sources(&sources)?);
        let (partials, squared_norms) = values.split_at(count as usize * directions);
        let evaluations = outs
            .into_iter()
            .zip(partials.chunks(count as usize))
            .map(|(buf, part)| {
                let total: f64 = part.iter().map(|v| f64::from(*v)).sum();
                (total / n as f64, GpuField { buf, grid })
            })
            .collect();
        let norms = squared_norms.iter().map(|v| f64::from(*v).sqrt()).collect();
        Ok((evaluations, norms))
    }

    /// [`Backend::smooth_field`] with the gather kernel on every axis, the pre-tiling
    /// implementation; the tiled kernels must match it bit for bit.
    #[doc(hidden)]
    pub fn smooth_field_gather(&self, field: &GpuField, variance: f64) -> RegResult<GpuField> {
        if variance <= 0.0 {
            return self.copy_field(field);
        }
        self.smooth_with(field, variance, false)
    }

    /// Whether the Gaussian of `variance` on `size` runs on the tiled kernels on every axis.
    #[doc(hidden)]
    pub fn smoothing_is_tiled(&self, variance: f64, size: [usize; 3]) -> RegResult<bool> {
        let taps = self.tap_set(variance, size)?;
        Ok(taps.radius.iter().all(|r| *r <= BOX_RADIUS_MAX))
    }

    fn smooth_with(&self, field: &GpuField, variance: f64, tiled: bool) -> RegResult<GpuField> {
        let grid = field.grid;
        let size = grid.size();
        let taps = self.tap_set(variance, size)?;
        let n = grid.num_voxels();
        let t1 = self.field_buffer(&grid)?;
        let t2 = self.field_buffer(&grid)?;
        let out = self.field_buffer(&grid)?;
        let w1 = if variance < 0.5 {
            1.0 - variance / 0.5
        } else {
            1.0
        };
        let w2 = 1.0 - w1;
        let strides = [1u32, size[0] as u32, (size[0] * size[1]) as u32];
        let other_strides = [0u32, (size[0] * size[1]) as u32, size[0] as u32];
        let [nx, ny, nz] = [size[0] as u32, size[1] as u32, size[2] as u32];
        let gather_groups = [nx.div_ceil(32), ny.div_ceil(4), nz.div_ceil(2)];
        let dims = [nx, ny, nz, n as u32];
        self.run_pass("smooth_field", |pass| {
            for axis in 0..3 {
                let (src, dst) = match axis {
                    0 => (field.buf.buffer(), t1.buffer()),
                    1 => (t1.buffer(), t2.buffer()),
                    _ => (t2.buffer(), out.buffer()),
                };
                let uniform = GaussU {
                    dims,
                    axis: axis as u32,
                    len: size[axis] as u32,
                    stride: strides[axis],
                    radius: taps.radius[axis],
                    tap_off: taps.offset[axis],
                    fin: u32::from(axis == 2),
                    other_stride: other_strides[axis],
                    pad: 0,
                    w1: w1 as f32,
                    w2: w2 as f32,
                    pad_f: [0.0; 2],
                };
                let buffers = [src, &taps.buf, dst, field.buf.buffer()];
                if !tiled || taps.radius[axis] > BOX_RADIUS_MAX {
                    self.launch(pass, &self.kernels.gauss, &buffers, &uniform, gather_groups)?;
                } else if axis == 0 {
                    self.launch(
                        pass,
                        &self.kernels.gauss_x,
                        &buffers,
                        &uniform,
                        [nx.div_ceil(256), ny, nz],
                    )?;
                } else {
                    let (segments, lines) = if axis == 1 { (ny, nz) } else { (nz, ny) };
                    self.launch(
                        pass,
                        &self.kernels.gauss_yz,
                        &buffers,
                        &uniform,
                        [nx.div_ceil(32), segments.div_ceil(32), lines],
                    )?;
                }
            }
            Ok(())
        })?;
        Ok(GpuField { buf: out, grid })
    }

    fn box_pass(
        &self,
        pass: &mut wgpu::ComputePass<'_>,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        grid: &Grid,
        axis: usize,
        radius: u32,
    ) -> RegResult<()> {
        let [nx, ny, nz] = grid.size();
        let (nxu, nyu, nzu) = (nx as u32, ny as u32, nz as u32);
        let n = grid.num_voxels() as u32;
        let (axis_len, axis_stride, other_stride) = match axis {
            0 => (nxu, 1, 0),
            1 => (nyu, nxu, nxu * nyu),
            _ => (nzu, nxu * nyu, nxu),
        };
        let params = BoxParams {
            nx: nxu,
            ny: nyu,
            nz: nzu,
            n,
            axis_len,
            axis_stride,
            other_stride,
            radius,
            planes: 1,
            pad: [0; 3],
        };
        match axis {
            0 => self.launch(
                pass,
                &self.kernels.box_x,
                &[src, dst],
                &params,
                [nxu.div_ceil(256), nyu, nzu],
            ),
            1 => self.launch(
                pass,
                &self.kernels.box_yz,
                &[src, dst],
                &params,
                [nxu.div_ceil(32), nyu.div_ceil(32), nzu],
            ),
            _ => self.launch(
                pass,
                &self.kernels.box_yz,
                &[src, dst],
                &params,
                [nxu.div_ceil(32), nzu.div_ceil(32), nyu],
            ),
        }
    }
}

impl Backend for WgpuBackend {
    type Image = GpuImage;
    type Field = GpuField;

    fn name(&self) -> String {
        format!(
            "wgpu-f32 ({}, {})",
            self.ctx.info.backend, self.ctx.info.name
        )
    }

    fn upload_image(&self, volume: &Volume<f64>) -> RegResult<GpuImage> {
        let grid = *volume.grid();
        let buf = self.image_buffer(&grid)?;
        let data: Vec<f32> = volume.data().iter().map(|v| *v as f32).collect();
        self.ctx
            .queue
            .write_buffer(buf.buffer(), 0, bytemuck::cast_slice(&data));
        self.uploads.fetch_add(1, Ordering::Relaxed);
        self.forget_trim();
        Ok(GpuImage { buf, grid })
    }

    fn zero_field(&self, grid: &Grid) -> RegResult<GpuField> {
        let buf = self.field_buffer(grid)?;
        self.run_encoder("zero_field", |enc| {
            enc.clear_buffer(buf.buffer(), 0, None);
            Ok(())
        })?;
        self.trim_pool_once_for(grid);
        Ok(GpuField { buf, grid: *grid })
    }

    fn upload_field(&self, field: &VectorField) -> RegResult<GpuField> {
        let grid = *field.grid();
        let n = grid.num_voxels();
        let buf = self.field_buffer(&grid)?;
        let mut data = vec![0.0f32; 3 * n];
        for (i, v) in field.data().iter().enumerate() {
            data[i] = v[0] as f32;
            data[n + i] = v[1] as f32;
            data[2 * n + i] = v[2] as f32;
        }
        self.ctx
            .queue
            .write_buffer(buf.buffer(), 0, bytemuck::cast_slice(&data));
        self.uploads.fetch_add(1, Ordering::Relaxed);
        self.forget_trim();
        Ok(GpuField { buf, grid })
    }

    fn download_field(&self, field: &GpuField) -> RegResult<VectorField> {
        let n = field.grid.num_voxels();
        self.field_downloads.fetch_add(1, Ordering::Relaxed);
        let data = self.ctx.read_f32(field.buf.buffer(), 3 * n).map_err(gerr)?;
        let vectors: Vec<Vec3> = (0..n)
            .map(|i| {
                [
                    f64::from(data[i]),
                    f64::from(data[n + i]),
                    f64::from(data[2 * n + i]),
                ]
            })
            .collect();
        VectorField::from_vec(field.grid, vectors)
    }

    fn field_grid(&self, field: &GpuField) -> Grid {
        field.grid
    }

    fn copy_field(&self, field: &GpuField) -> RegResult<GpuField> {
        let buf = self.field_buffer(&field.grid)?;
        let bytes = field.grid.num_voxels() as u64 * 12;
        self.run_encoder("copy_field", |enc| {
            enc.copy_buffer_to_buffer(field.buf.buffer(), 0, buf.buffer(), 0, bytes);
            Ok(())
        })?;
        Ok(GpuField {
            buf,
            grid: field.grid,
        })
    }

    fn warp_through_field(
        &self,
        source: &GpuImage,
        field: &GpuField,
        affine: Option<&AffineTransform>,
    ) -> RegResult<GpuImage> {
        let grid = field.grid;
        let out = self.image_buffer(&grid)?;
        let uniform = warp_uniform(&grid, &source.grid, affine, true, 1)?;
        self.run_pass("warp", |pass| {
            self.launch(
                pass,
                &self.kernels.warp,
                &[source.buf.buffer(), field.buf.buffer(), out.buffer()],
                &uniform,
                self.groups_3d(&grid),
            )
        })?;
        Ok(GpuImage { buf: out, grid })
    }

    fn cc_value_derivative(
        &self,
        fixed: &GpuImage,
        moving: &GpuImage,
        radius: usize,
    ) -> RegResult<(f64, GpuField)> {
        let (mut out, _) = self.cc_evaluate(fixed, moving, radius, false, false)?;
        out.pop().ok_or_else(|| bad("cc produced no result"))
    }

    fn cc_value_derivative_pair(
        &self,
        fixed: &GpuImage,
        moving: &GpuImage,
        radius: usize,
    ) -> RegResult<MetricPair<GpuField>> {
        let (out, _) = self.cc_evaluate(fixed, moving, radius, true, false)?;
        cc_pair(out)
    }

    fn cc_value_derivative_pair_with_norms(
        &self,
        fixed: &GpuImage,
        moving: &GpuImage,
        radius: usize,
    ) -> RegResult<(MetricPair<GpuField>, [f64; 2])> {
        let (out, norms) = self.cc_evaluate(fixed, moving, radius, true, true)?;
        let norms: [f64; 2] = norms
            .try_into()
            .map_err(|_| bad("cc pair produced fewer than two norms"))?;
        Ok((cc_pair(out)?, norms))
    }

    fn smooth_field(&self, field: GpuField, variance: f64) -> RegResult<GpuField> {
        if variance <= 0.0 {
            return Ok(field);
        }
        self.smooth_with(&field, variance, true)
    }

    fn max_scaled_norm(&self, field: &GpuField) -> RegResult<f64> {
        Ok(f64::from(self.reduce_field(field, 0, "max_scaled_norm")?))
    }

    fn max_scaled_norm_pair(&self, a: &GpuField, b: &GpuField) -> RegResult<(f64, f64)> {
        if a.grid != b.grid {
            return Ok((self.max_scaled_norm(a)?, self.max_scaled_norm(b)?));
        }
        let part = self.reduction_partials(&a.grid)?;
        let scalars = [
            self.acquire("reduction scalar", 16)?,
            self.acquire("reduction scalar", 16)?,
        ];
        self.run_pass("max_scaled_norm_pair", |pass| {
            for (field, scalar) in [a, b].into_iter().zip(&scalars) {
                self.encode_reduce(
                    pass,
                    field.buf.buffer(),
                    &field.grid,
                    0,
                    part.buffer(),
                    scalar.buffer(),
                )?;
            }
            Ok(())
        })?;
        let bytes = self.read_sources(&[(scalars[0].buffer(), 4), (scalars[1].buffer(), 4)])?;
        let v: Vec<f32> = bytemuck::pod_collect_to_vec(&bytes);
        Ok((f64::from(v[0]), f64::from(v[1])))
    }

    fn l2_norm(&self, field: &GpuField) -> RegResult<f64> {
        Ok(f64::from(self.reduce_field(field, 1, "l2_norm")?).sqrt())
    }

    fn scale(&self, field: &mut GpuField, s: f64) -> RegResult<()> {
        let total = field.grid.num_voxels() as u64 * 3;
        let groups = self.groups_1d(total)?;
        self.run_pass("scale", |pass| {
            self.launch(
                pass,
                &self.kernels.scale,
                &[field.buf.buffer()],
                &ElementU {
                    total: total as u32,
                    a: s as f32,
                    pad: [0; 2],
                },
                groups,
            )
        })
    }

    fn axpy(&self, acc: &mut GpuField, a: f64, x: &GpuField) -> RegResult<()> {
        self.require_same_grid("axpy", &acc.grid, &x.grid)?;
        let total = acc.grid.num_voxels() as u64 * 3;
        let groups = self.groups_1d(total)?;
        self.run_pass("axpy", |pass| {
            self.launch(
                pass,
                &self.kernels.axpy,
                &[x.buf.buffer(), acc.buf.buffer()],
                &ElementU {
                    total: total as u32,
                    a: a as f32,
                    pad: [0; 2],
                },
                groups,
            )
        })
    }

    fn compose(&self, displacement: &GpuField, warping: &GpuField) -> RegResult<GpuField> {
        self.require_same_grid("compose", &displacement.grid, &warping.grid)?;
        let grid = warping.grid;
        let out = self.field_buffer(&grid)?;
        let ifo = index_from_offset(&grid)?;
        let uniform = ComposeU {
            dims: dims4(&grid),
            l: [row4(&ifo, 0, 0.0), row4(&ifo, 1, 0.0), row4(&ifo, 2, 0.0)],
        };
        self.run_pass("compose", |pass| {
            self.launch(
                pass,
                &self.kernels.compose,
                &[
                    displacement.buf.buffer(),
                    warping.buf.buffer(),
                    out.buffer(),
                ],
                &uniform,
                self.groups_3d(&grid),
            )
        })?;
        Ok(GpuField { buf: out, grid })
    }

    fn invert(
        &self,
        field: &GpuField,
        initial: GpuField,
        params: &InvertParams,
    ) -> RegResult<(GpuField, InvertStats)> {
        let (psi, state) = self.invert_encode(field, initial, params)?;
        let words: Vec<u32> = match state {
            Some(state) => {
                bytemuck::pod_collect_to_vec(&self.read_scalar_bytes(state.buffer(), 16)?)
            }
            None => vec![0; 4],
        };
        Ok((psi, invert_stats(&words)))
    }

    fn invert_pair(
        &self,
        field: GpuField,
        inverse: GpuField,
        params: &InvertParams,
    ) -> RegResult<InversionPair<GpuField>> {
        let (inverse, inverse_state) = self.invert_encode(&field, inverse, params)?;
        let (field, field_state) = self.invert_encode(&inverse, field, params)?;
        let words: Vec<u32> = match (&inverse_state, &field_state) {
            (Some(a), Some(b)) => bytemuck::pod_collect_to_vec(
                &self.read_sources(&[(a.buffer(), 16), (b.buffer(), 16)])?,
            ),
            _ => vec![0; 8],
        };
        Ok((
            (inverse, invert_stats(&words[..4])),
            (field, invert_stats(&words[4..])),
        ))
    }

    fn resample_field(&self, field: &GpuField, grid: &Grid) -> RegResult<GpuField> {
        if field.grid == *grid {
            return self.copy_field(field);
        }
        let out = self.field_buffer(grid)?;
        let uniform = warp_uniform(grid, &field.grid, None, false, 3)?;
        self.run_pass("resample_field", |pass| {
            self.launch(
                pass,
                &self.kernels.warp,
                &[field.buf.buffer(), field.buf.buffer(), out.buffer()],
                &uniform,
                self.groups_3d(grid),
            )
        })?;
        self.trim_pool_for(grid);
        self.forget_trim();
        Ok(GpuField {
            buf: out,
            grid: *grid,
        })
    }
}

impl WgpuBackend {
    /// Records ITK's fixed-point inversion of `field` from `initial` (updated in place and
    /// returned) and returns the device buffer of its state words (maximum and mean error, steps
    /// executed), `None` when `params` allows no step.
    fn invert_encode(
        &self,
        field: &GpuField,
        initial: GpuField,
        params: &InvertParams,
    ) -> RegResult<(GpuField, Option<PoolBuf>)> {
        self.require_same_grid("invert", &field.grid, &initial.grid)?;
        let grid = field.grid;
        let n = grid.num_voxels();
        let psi = initial;
        let err = self.field_buffer(&grid)?;
        let g3 = self.groups_3d(&grid);
        let count = g3[0] * g3[1] * g3[2];
        let part = self.acquire("invert partials", u64::from(count) * 8)?;
        let state = self.acquire("invert state", 16)?;
        let init: [u32; 4] = [f32::MAX.to_bits(), f32::MAX.to_bits(), 0, 1];
        self.ctx
            .queue
            .write_buffer(state.buffer(), 0, bytemuck::bytes_of(&init));
        let ifo = index_from_offset(&grid)?;
        let l = [row4(&ifo, 0, 0.0), row4(&ifo, 1, 0.0), row4(&ifo, 2, 0.0)];
        let sp = spacing4(&grid);
        let dims = dims4(&grid);
        let max_tol = params.max_error_tolerance;
        let mean_tol = params.mean_error_tolerance;
        let mut first = 0u32;
        while first < params.max_iterations {
            let take = INVERT_BODIES_PER_SUBMIT.min(params.max_iterations - first);
            let label = if first == 0 {
                "invert"
            } else {
                "invert (continued)"
            };
            self.run_pass(label, |pass| {
                let begin = self.prepare(
                    &self.kernels.inv_begin,
                    &[state.buffer()],
                    &InvBeginU {
                        max_tol: max_tol as f32,
                        mean_tol: mean_tol as f32,
                        pad: [0; 2],
                    },
                )?;
                let residual = self.prepare(
                    &self.kernels.inv_residual,
                    &[
                        field.buf.buffer(),
                        psi.buf.buffer(),
                        err.buffer(),
                        part.buffer(),
                        state.buffer(),
                    ],
                    &InvResidualU { dims, l, sp },
                )?;
                let reduce = self.prepare(
                    &self.kernels.inv_reduce,
                    &[part.buffer(), state.buffer()],
                    &InvReduceU {
                        count,
                        n: n as u32,
                        pad: [0; 2],
                    },
                )?;
                let update_group = |eps: f32| {
                    self.prepare(
                        &self.kernels.inv_update,
                        &[psi.buf.buffer(), err.buffer(), state.buffer()],
                        &InvUpdateU {
                            dims,
                            sp,
                            eps,
                            enforce: u32::from(params.enforce_boundary_condition),
                            pad: [0; 2],
                        },
                    )
                };
                let update_first = (first == 0).then(|| update_group(0.75)).transpose()?;
                let update_rest = update_group(0.5)?;
                for body in first..first + take {
                    let update = match (&update_first, body) {
                        (Some(group), 0) => group,
                        _ => &update_rest,
                    };
                    self.dispatch(pass, &self.kernels.inv_begin, &begin, [1, 1, 1])?;
                    self.dispatch(pass, &self.kernels.inv_residual, &residual, g3)?;
                    self.dispatch(pass, &self.kernels.inv_reduce, &reduce, [1, 1, 1])?;
                    self.dispatch(pass, &self.kernels.inv_update, update, g3)?;
                }
                Ok(())
            })?;
            first += take;
        }
        Ok((psi, (params.max_iterations > 0).then_some(state)))
    }
}

fn cc_pair(mut out: Vec<MetricEvaluation<GpuField>>) -> RegResult<MetricPair<GpuField>> {
    match (out.pop(), out.pop()) {
        (Some(second), Some(first)) => Ok((first, second)),
        _ => Err(bad("cc pair produced fewer than two results")),
    }
}

fn invert_stats(words: &[u32]) -> InvertStats {
    let max_error = f64::from(f32::from_bits(words[0]));
    let mean_error = f64::from(f32::from_bits(words[1]));
    let executed = words[2];
    InvertStats {
        iterations: executed,
        mean_error_norm: if executed == 0 { 0.0 } else { mean_error },
        max_error_norm: if executed == 0 { 0.0 } else { max_error },
    }
}
