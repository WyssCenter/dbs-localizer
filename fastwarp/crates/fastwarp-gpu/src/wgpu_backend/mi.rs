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
// Portions derived from ITK `itkMattesMutualInformationImageToImageMetricv4GetValueAndDerivativeThreader.hxx`
// (`ProcessPoint`: moving bin, cubic B-spline Parzen weights over four bins, derivative weights),
// `itkMattesMutualInformationImageToImageMetricv4.hxx` (`ComputeResults`: derivative as the sum over
// joint cells of the Jacobian-weighted PDF derivative times `log(p(f,m) / p_M(m))`) and
// `itkCentralDifferenceImageFunction.hxx`. The GPU evaluation (fixed-point integer histogram,
// per-workgroup partials, ratio table, per-sample derivative reduction) is this crate's own. See
// `docs/THIRD_PARTY_PORTS.md`.

//! Mattes mutual information on the GPU for the linear stages (`k_mi_hist`, `k_mi_merge`,
//! `k_mi_deriv`).
//!
//! One evaluation is a histogram pass (every workgroup accumulates `2^-20` fixed-point weights of
//! its chunk of samples into a private `u32` histogram, so the result does not depend on the
//! order of the atomic additions), a one-workgroup merge of the partial histograms in workgroup
//! order with a 64-bit carry, and a read-back of 1056 integer pairs. The host forms the joint
//! PDF, the value and the per-cell weights `nFactor * log(p(f,m) / p_M(m))` with the same code as
//! the CPU reference. The derivative needs a second pass: each sample contributes
//! `sum_k B3'(arg_k) w[f, m_k] * (grad M . dT/dtheta_p)` to every parameter `p`, reduced per
//! workgroup by a fixed tree and summed on the host in workgroup order.

use fastwarp_reg::error::Result as RegResult;
use fastwarp_reg::grid::Grid;
use fastwarp_reg::linalg::{mat3_mul, mat3_mul_vec, Mat3, Vec3};
use fastwarp_reg::linear::{LinearEngine, PreparedSamples};
use fastwarp_reg::metric::mi::{finish_histogram, MiBinning};
use fastwarp_reg::optimizer::LinearMetric;
use fastwarp_reg::transform::{AffineTransform, LinearMovingTransform};
use fastwarp_reg::volume::Volume;
use fastwarp_reg::Backend;

use super::{bad, gerr, index_from_offset, GpuImage, PoolBuf, WgpuBackend};
use crate::device::GpuContext;
use crate::error::GpuResult;

const COMMON: &str = include_str!("../shaders/mi_common.wgsl");
const HIST: &str = include_str!("../shaders/mi_hist.wgsl");
const DERIV: &str = include_str!("../shaders/mi_deriv.wgsl");
const MERGE: &str = include_str!("../shaders/mi_merge.wgsl");
const MERGE_FINAL: &str = include_str!("../shaders/mi_merge_final.wgsl");

const STRIDE: usize = 1056;
const MAX_STRIPES: u32 = 64;
const BINS: usize = 32;
const CHUNK: u32 = 2048;
const MAX_CHUNK: u32 = 4096;
const WORKGROUP: u32 = 256;
const FIXED_POINT: f64 = 1_048_576.0;
const MAX_PARAMS: usize = 12;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MiU {
    dims: [u32; 4],
    cfg: [u32; 4],
    t0: [f32; 4],
    t1: [f32; 4],
    t2: [f32; 4],
    ox: [f32; 4],
    oy: [f32; 4],
    oz: [f32; 4],
    gscale: [f32; 4],
    rng: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MergeU {
    nwg: u32,
    per_stripe: u32,
    stripes: u32,
    pad: u32,
}

pub(super) struct MiKernels {
    hist: wgpu::ComputePipeline,
    deriv: wgpu::ComputePipeline,
    merge: wgpu::ComputePipeline,
    merge_final: wgpu::ComputePipeline,
}

impl MiKernels {
    pub(super) fn new(ctx: &GpuContext) -> GpuResult<Self> {
        let with_common = |body: &str| format!("{COMMON}\n{body}");
        Ok(Self {
            hist: ctx.compile("k_mi_hist", &with_common(HIST), "k_mi_hist")?,
            deriv: ctx.compile("k_mi_deriv", &with_common(DERIV), "k_mi_deriv")?,
            merge: ctx.compile("k_mi_merge_stripe", MERGE, "k_mi_merge_stripe")?,
            merge_final: ctx.compile("k_mi_merge_final", MERGE_FINAL, "k_mi_merge_final")?,
        })
    }
}

/// The Mattes metric of one level of a linear stage, resident on the device: the smoothed moving
/// image, the sample points with their fixed bins, and the work buffers.
pub struct GpuMiLevel<'a> {
    backend: &'a WgpuBackend,
    samples: PoolBuf,
    moving: GpuImage,
    partials: PoolBuf,
    stripe_sums: PoolBuf,
    merged: PoolBuf,
    staging: wgpu::Buffer,
    ratio: PoolBuf,
    gk: PoolBuf,
    dpart: PoolBuf,
    deriv_staging: wgpu::Buffer,
    count: usize,
    groups: u32,
    per_stripe: u32,
    stripes: u32,
    chunk: u32,
    grid: Grid,
    index_from_offset: Mat3,
    binning: MiBinning,
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn deriv_bytes(groups: u32) -> u64 {
    u64::from(groups) * MAX_PARAMS as u64 * 4
}

fn v4(v: Vec3, w: f64) -> [f32; 4] {
    [v[0] as f32, v[1] as f32, v[2] as f32, w as f32]
}

impl<'a> GpuMiLevel<'a> {
    fn new(
        backend: &'a WgpuBackend,
        moving: &Volume<f64>,
        samples: &PreparedSamples,
        chunk: u32,
    ) -> RegResult<Self> {
        if chunk == 0 || !chunk.is_multiple_of(WORKGROUP) || chunk > MAX_CHUNK {
            return Err(bad(format!(
                "the sample chunk must be a positive multiple of {WORKGROUP} of at most \
                 {MAX_CHUNK}, not {chunk}"
            )));
        }
        if samples.binning.bins != BINS {
            return Err(bad(format!(
                "the GPU histogram kernels support {BINS} bins, not {}",
                samples.binning.bins
            )));
        }
        let count = samples.points.len();
        if count == 0 {
            return Err(bad("no samples inside the fixed image"));
        }
        let groups = (count as u32).div_ceil(chunk);
        let per_stripe = groups.div_ceil(MAX_STRIPES);
        let stripes = groups.div_ceil(per_stripe);
        let records: Vec<[u32; 4]> = samples
            .points
            .iter()
            .zip(&samples.fixed_bin)
            .map(|(p, b)| {
                [
                    (p[0] as f32).to_bits(),
                    (p[1] as f32).to_bits(),
                    (p[2] as f32).to_bits(),
                    *b,
                ]
            })
            .collect();
        let sample_buf = backend.acquire("mi samples", (count * 16) as u64)?;
        backend
            .ctx
            .queue
            .write_buffer(sample_buf.buffer(), 0, bytemuck::cast_slice(&records));
        let moving_img = backend.upload_image(moving)?;
        let grid = *moving.grid();
        Ok(Self {
            backend,
            samples: sample_buf,
            moving: moving_img,
            partials: backend.acquire("mi partials", u64::from(groups) * STRIDE as u64 * 4)?,
            stripe_sums: backend
                .acquire("mi stripe sums", u64::from(stripes) * STRIDE as u64 * 8)?,
            merged: backend.acquire("mi merged", (STRIDE * 8) as u64)?,
            staging: backend
                .ctx
                .staging_buffer((STRIDE * 8) as u64)
                .map_err(gerr)?,
            ratio: backend.acquire("mi ratio", (BINS * BINS * 4) as u64)?,
            gk: backend.acquire("mi jacobian", (MAX_PARAMS * 3 * 16) as u64)?,
            dpart: backend.acquire("mi derivative partials", deriv_bytes(groups))?,
            deriv_staging: backend
                .ctx
                .staging_buffer(deriv_bytes(groups))
                .map_err(gerr)?,
            count,
            groups,
            per_stripe,
            stripes,
            chunk,
            grid,
            index_from_offset: index_from_offset(&grid)?,
            binning: samples.binning,
        })
    }

    fn uniform(&self, t: &LinearMovingTransform, params: usize) -> MiU {
        let total = AffineTransform::compose(&t.outer, &t.optimised.to_affine());
        let mi = &self.index_from_offset;
        let a = mat3_mul(mi, &total.matrix);
        let b = mat3_mul_vec(mi, sub(total.offset(), self.grid.origin()));
        let sp = self.grid.spacing();
        let half = |d: usize| -> Vec3 {
            [
                0.5 * sp[d] * mi[0][d],
                0.5 * sp[d] * mi[1][d],
                0.5 * sp[d] * mi[2][d],
            ]
        };
        let s = self.grid.size();
        let bn = &self.binning;
        MiU {
            dims: [s[0] as u32, s[1] as u32, s[2] as u32, self.count as u32],
            cfg: [BINS as u32, self.chunk, params as u32, self.groups],
            t0: v4(a[0], b[0]),
            t1: v4(a[1], b[1]),
            t2: v4(a[2], b[2]),
            ox: v4(half(0), 0.0),
            oy: v4(half(1), 0.0),
            oz: v4(half(2), 0.0),
            gscale: [
                (1.0 / sp[0]) as f32,
                (1.0 / sp[1]) as f32,
                (1.0 / sp[2]) as f32,
                0.0,
            ],
            rng: [
                bn.moving_min as f32,
                bn.moving_max as f32,
                bn.moving_bin_size as f32,
                bn.moving_normalized_min as f32,
            ],
        }
    }

    fn histogram(&self, u: &MiU) -> RegResult<(Vec<f64>, Vec<f64>, usize)> {
        let be = self.backend;
        let k = &be.mi_kernels;
        let merge = MergeU {
            nwg: self.groups,
            per_stripe: self.per_stripe,
            stripes: self.stripes,
            pad: 0,
        };
        let bytes = (STRIDE * 8) as u64;
        be.run_encoder("mi_hist", |enc| {
            {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("mi_hist"),
                    timestamp_writes: None,
                });
                be.launch(
                    &mut pass,
                    &k.hist,
                    &[
                        self.samples.buffer(),
                        self.moving.buf.buffer(),
                        self.partials.buffer(),
                    ],
                    u,
                    [self.groups, 1, 1],
                )?;
                be.launch(
                    &mut pass,
                    &k.merge,
                    &[self.partials.buffer(), self.stripe_sums.buffer()],
                    &merge,
                    [self.stripes, 1, 1],
                )?;
                be.launch(
                    &mut pass,
                    &k.merge_final,
                    &[self.stripe_sums.buffer(), self.merged.buffer()],
                    &merge,
                    [1, 1, 1],
                )?;
            }
            enc.copy_buffer_to_buffer(self.merged.buffer(), 0, &self.staging, 0, bytes);
            Ok(())
        })?;
        be.scalar_readbacks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let raw: Vec<u32> =
            bytemuck::pod_collect_to_vec(&be.ctx.read_staging(&self.staging, bytes).map_err(gerr)?);
        let cell = |i: usize| -> u64 { u64::from(raw[2 * i]) | (u64::from(raw[2 * i + 1]) << 32) };
        let joint: Vec<f64> = (0..BINS * BINS)
            .map(|i| cell(i) as f64 / FIXED_POINT)
            .collect();
        let marginal: Vec<f64> = (0..BINS).map(|i| cell(BINS * BINS + i) as f64).collect();
        let valid = (0..BINS).map(|i| cell(BINS * BINS + i)).sum::<u64>() as usize;
        Ok((joint, marginal, valid))
    }

    fn jacobian_rows(&self, t: &LinearMovingTransform) -> Vec<[f32; 4]> {
        let n = t.optimised.number_of_parameters();
        let origin = [0.0; 3];
        let j0 = t.jacobian(origin);
        let axes = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let ja: Vec<Vec<Vec3>> = axes.iter().map(|e| t.jacobian(*e)).collect();
        let mut rows = vec![[0.0f32; 4]; MAX_PARAMS * 3];
        for k in 0..n {
            for r in 0..3 {
                rows[3 * k + r] = [
                    (ja[0][k][r] - j0[k][r]) as f32,
                    (ja[1][k][r] - j0[k][r]) as f32,
                    (ja[2][k][r] - j0[k][r]) as f32,
                    j0[k][r] as f32,
                ];
            }
        }
        rows
    }
}

impl LinearMetric for GpuMiLevel<'_> {
    fn value(&self, t: &LinearMovingTransform) -> RegResult<f64> {
        let u = self.uniform(t, t.optimised.number_of_parameters());
        let (joint, marginal, valid) = self.histogram(&u)?;
        Ok(finish_histogram(
            BINS,
            self.binning.moving_bin_size,
            valid,
            joint,
            marginal,
            false,
        )?
        .value)
    }

    fn value_and_derivative(&self, t: &LinearMovingTransform) -> RegResult<(f64, Vec<f64>)> {
        let params = t.optimised.number_of_parameters();
        if params > MAX_PARAMS {
            return Err(bad("more than twelve parameters"));
        }
        let u = self.uniform(t, params);
        let (joint, marginal, valid) = self.histogram(&u)?;
        let fin = finish_histogram(
            BINS,
            self.binning.moving_bin_size,
            valid,
            joint,
            marginal,
            true,
        )?;
        let be = self.backend;
        let ratio: Vec<f32> = fin.ratio.iter().map(|r| *r as f32).collect();
        be.ctx
            .queue
            .write_buffer(self.ratio.buffer(), 0, bytemuck::cast_slice(&ratio));
        be.ctx.queue.write_buffer(
            self.gk.buffer(),
            0,
            bytemuck::cast_slice(&self.jacobian_rows(t)),
        );
        be.run_pass("mi_deriv", |pass| {
            be.launch(
                pass,
                &be.mi_kernels.deriv,
                &[
                    self.samples.buffer(),
                    self.moving.buf.buffer(),
                    self.ratio.buffer(),
                    self.gk.buffer(),
                    self.dpart.buffer(),
                ],
                &u,
                [self.groups, 1, 1],
            )
        })?;
        be.scalar_readbacks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let parts: Vec<f32> = bytemuck::pod_collect_to_vec(
            &be.ctx
                .read_bytes_staged(
                    self.dpart.buffer(),
                    0,
                    deriv_bytes(self.groups),
                    &self.deriv_staging,
                )
                .map_err(gerr)?,
        );
        let mut derivative = vec![0.0f64; params];
        for g in 0..self.groups as usize {
            for (p, d) in derivative.iter_mut().enumerate() {
                *d += f64::from(parts[g * MAX_PARAMS + p]);
            }
        }
        Ok((fin.value, derivative))
    }
}

impl LinearEngine for WgpuBackend {
    type Level<'a>
        = GpuMiLevel<'a>
    where
        Self: 'a;

    fn name(&self) -> String {
        format!("{} Mattes MI", Backend::name(self))
    }

    fn prepare_level<'a>(
        &'a self,
        _fixed: Volume<f64>,
        moving: Volume<f64>,
        samples: PreparedSamples,
    ) -> RegResult<GpuMiLevel<'a>> {
        GpuMiLevel::new(self, &moving, &samples, CHUNK)
    }

    fn prepare_level_borrowed<'a>(
        &'a self,
        _fixed: &'a Volume<f64>,
        moving: &'a Volume<f64>,
        samples: PreparedSamples,
    ) -> RegResult<GpuMiLevel<'a>> {
        GpuMiLevel::new(self, moving, &samples, CHUNK)
    }
}

impl WgpuBackend {
    /// Like `prepare_level`, with `chunk` samples per workgroup (a positive multiple of 256) instead
    /// of the production length, so a small image still spans several workgroups.
    pub fn prepare_level_with_chunk(
        &self,
        moving: Volume<f64>,
        samples: PreparedSamples,
        chunk: u32,
    ) -> RegResult<GpuMiLevel<'_>> {
        GpuMiLevel::new(self, &moving, &samples, chunk)
    }
}
