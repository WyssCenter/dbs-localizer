// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::time::Instant;

use serde::Serialize;

use crate::device::GpuContext;
use crate::error::{GpuError, GpuResult};

/// How a duration was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TimingMethod {
    GpuTimestamps,
    WallClock,
}

impl TimingMethod {
    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            TimingMethod::GpuTimestamps => "gpu-timestamps (pass-bracketed)",
            TimingMethod::WallClock => "wall-clock (submit + device poll wait)",
        }
    }
}

/// Duration of one submission. `seconds` is the primary figure: GPU timestamps when available,
/// otherwise the wall-clock time. `wall_seconds` is always the wall-clock time of submit + wait.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Timing {
    pub seconds: f64,
    pub wall_seconds: f64,
    pub method: TimingMethod,
}

pub(crate) struct Profiler {
    query_set: Option<wgpu::QuerySet>,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
}

impl Profiler {
    pub(crate) fn new(device: &wgpu::Device, features: wgpu::Features) -> Self {
        let query_set = features.contains(wgpu::Features::TIMESTAMP_QUERY).then(|| {
            device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("fastwarp-gpu timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: 2,
            })
        });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fastwarp-gpu timestamp resolve"),
            size: 16,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fastwarp-gpu timestamp read"),
            size: 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Profiler {
            query_set,
            resolve,
            read,
        }
    }
}

impl GpuContext {
    /// Whether GPU timestamps are available.
    pub fn has_timestamps(&self) -> bool {
        self.profiler.query_set.is_some()
    }

    /// Records the commands of `f` between two timestamp-bracketing empty passes, submits them,
    /// waits, and returns the duration. Falls back to wall-clock when `TIMESTAMP_QUERY` is absent
    /// or `force_wall` is set.
    pub fn timed(
        &self,
        force_wall: bool,
        f: impl FnOnce(&mut wgpu::CommandEncoder),
    ) -> GpuResult<Timing> {
        let use_ts = !force_wall && self.profiler.query_set.is_some();
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fastwarp-gpu timed"),
            });
        if use_ts {
            if let Some(qs) = &self.profiler.query_set {
                drop(enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("ts begin"),
                    timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                        query_set: qs,
                        beginning_of_pass_write_index: Some(0),
                        end_of_pass_write_index: None,
                    }),
                }));
            }
        }
        f(&mut enc);
        if use_ts {
            if let Some(qs) = &self.profiler.query_set {
                drop(enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("ts end"),
                    timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                        query_set: qs,
                        beginning_of_pass_write_index: None,
                        end_of_pass_write_index: Some(1),
                    }),
                }));
                enc.resolve_query_set(qs, 0..2, &self.profiler.resolve, 0);
                enc.copy_buffer_to_buffer(&self.profiler.resolve, 0, &self.profiler.read, 0, 16);
            }
        }
        let cb = enc.finish();
        self.sync()?;
        let t0 = Instant::now();
        self.queue.submit(Some(cb));
        self.sync()?;
        let wall = t0.elapsed().as_secs_f64();
        if !use_ts {
            return Ok(Timing {
                seconds: wall,
                wall_seconds: wall,
                method: TimingMethod::WallClock,
            });
        }
        let bytes = self.map_read_all(&self.profiler.read, 16)?;
        let mut t = [0u64; 2];
        for (i, v) in t.iter_mut().enumerate() {
            let mut w = [0u8; 8];
            w.copy_from_slice(&bytes[i * 8..i * 8 + 8]);
            *v = u64::from_le_bytes(w);
        }
        if t[1] <= t[0] {
            return Ok(Timing {
                seconds: wall,
                wall_seconds: wall,
                method: TimingMethod::WallClock,
            });
        }
        let period = f64::from(self.queue.get_timestamp_period());
        Ok(Timing {
            seconds: (t[1] - t[0]) as f64 * period * 1e-9,
            wall_seconds: wall,
            method: TimingMethod::GpuTimestamps,
        })
    }

    /// Maps the first `len` bytes of a staging buffer (from [`Self::staging_buffer`]) that an
    /// already submitted copy filled, and returns them.
    pub fn read_staging(&self, staging: &wgpu::Buffer, len: u64) -> GpuResult<Vec<u8>> {
        self.map_read_all(staging, len)
    }

    fn map_read_all(&self, buffer: &wgpu::Buffer, len: u64) -> GpuResult<Vec<u8>> {
        let slice = buffer.slice(..len);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        if let Err(e) = self.sync() {
            match rx.try_recv() {
                Ok(Err(_)) => {}
                Ok(Ok(())) | Err(_) => buffer.unmap(),
            }
            return Err(e);
        }
        match rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(GpuError::Readback(e.to_string())),
            Err(e) => return Err(GpuError::Readback(e.to_string())),
        }
        let data = slice.get_mapped_range().map(|view| view.to_vec());
        buffer.unmap();
        data.map_err(|e| GpuError::Readback(e.to_string()))
    }

    /// Creates a host-readable staging buffer of `len` bytes for [`Self::read_bytes_staged`].
    pub fn staging_buffer(&self, len: u64) -> GpuResult<wgpu::Buffer> {
        self.scoped("readback staging", len, || {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fastwarp-gpu readback"),
                size: len,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        })
    }

    /// Copies `len` bytes of `buffer` starting at `offset` to the host.
    pub fn read_bytes(&self, buffer: &wgpu::Buffer, offset: u64, len: u64) -> GpuResult<Vec<u8>> {
        let staging = self.staging_buffer(len)?;
        self.read_bytes_staged(buffer, offset, len, &staging)
    }

    /// Like [`Self::read_bytes`] through a caller-owned `staging` buffer of at least `len` bytes.
    pub fn read_bytes_staged(
        &self,
        buffer: &wgpu::Buffer,
        offset: u64,
        len: u64,
        staging: &wgpu::Buffer,
    ) -> GpuResult<Vec<u8>> {
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fastwarp-gpu readback"),
            });
        enc.copy_buffer_to_buffer(buffer, offset, staging, 0, len);
        self.queue.submit(Some(enc.finish()));
        self.map_read_all(staging, len)
    }

    /// Reads `count` f32 values from the start of `buffer` through a caller-owned staging buffer.
    pub fn read_f32_staged(
        &self,
        buffer: &wgpu::Buffer,
        count: usize,
        staging: &wgpu::Buffer,
    ) -> GpuResult<Vec<f32>> {
        let bytes = self.read_bytes_staged(buffer, 0, (count * 4) as u64, staging)?;
        Ok(bytemuck::pod_collect_to_vec(&bytes))
    }

    /// Reads `count` f32 values from the start of `buffer`.
    pub fn read_f32(&self, buffer: &wgpu::Buffer, count: usize) -> GpuResult<Vec<f32>> {
        let bytes = self.read_bytes(buffer, 0, (count * 4) as u64)?;
        Ok(bytemuck::pod_collect_to_vec(&bytes))
    }

    /// Reads `count` u32 values from the start of `buffer`.
    pub fn read_u32(&self, buffer: &wgpu::Buffer, count: usize) -> GpuResult<Vec<u32>> {
        let bytes = self.read_bytes(buffer, 0, (count * 4) as u64)?;
        Ok(bytemuck::pod_collect_to_vec(&bytes))
    }
}
