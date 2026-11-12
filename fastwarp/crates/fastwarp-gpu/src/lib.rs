// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! wgpu compute device layer for the GPU registration engine, plus the WP1 go/no-go probe.

#[cfg(feature = "dev-tools")]
pub mod ab;
pub mod capacity;
pub mod caps;
pub mod device;
pub mod error;
#[cfg(feature = "dev-tools")]
pub mod gates;
pub mod gpu_n4;
pub mod kernels;
pub mod n4_kernels;
pub mod parity;
pub mod pool;
#[cfg(feature = "dev-tools")]
pub mod probe;
pub mod profiler;
#[cfg(feature = "dev-tools")]
pub mod reference;
pub mod syn_kernels;
pub mod wgpu_backend;

pub use capacity::{
    memory_budget, plan_capacity, plan_capacity_pair, BudgetSource, CapacityCaps, CapacityError,
    CapacityPlan, DeviceMemory, MemoryFigure,
};
pub use caps::{CapCheck, GpuCaps, Verdict};
pub use device::{AdapterRecord, BackendChoice, BackendSelect, GpuContext};
pub use error::{GpuError, GpuResult};
pub use gpu_n4::{GpuN4, N4SelfTestRecord};
pub use pool::BufferPool;
pub use profiler::{Timing, TimingMethod};
pub use wgpu_backend::{
    BackendStats, GpuField, GpuImage, GpuMiLevel, OpProfile, ProfileReport, SelfTestRecord,
    WgpuBackend,
};
