// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! Pre-flight capacity check for a registration grid.
//!
//! Pure and device-free: given the finest level's grid, the channel count and the device's
//! binding limit and memory budget, decide whether the level fits before any image is loaded. A
//! caller uses this to fall back to another backend (or fail with a remedy) instead of dying at
//! the finest SyN level mid-run.

use crate::caps::GpuCaps;
use crate::device::GpuContext;

static FALLBACK_BUDGET_NOTE: std::sync::Once = std::sync::Once::new();

/// The largest single storage binding of the split layout is the three-plane displacement field,
/// `n · 3 · 4` bytes (`wgpu_backend.rs`, `field_buffer`). WP8 split the five-plane CC buffer into
/// five `n · 4` bindings, so the field is now the largest.
const LARGEST_BINDING_BYTES_PER_VOXEL: u64 = 3 * 4;
/// Peak device allocation per voxel of the finest level's grid, two channels: the buffer pool's
/// high-water mark of a two-level `run_syn` on a 128^3 grid measured 180.2 B/voxel (the pool keeps
/// each size class at its own peak: about nine fields at `n * 12` and eighteen images at `n * 4`),
/// rounded up. It includes the full-resolution smoothed images of a finest level that is not
/// shrunk (`tests/syn_parity.rs`, `the_capacity_plan_bounds_the_pool_peak_of_a_two_level_run`).
const LEVEL_BYTES_PER_VOXEL_TWO_CHANNELS: u64 = 188;
/// What a second channel adds (two smoothed and two warped images, with the derivative fields
/// accumulated one channel at a time); one channel measured 165.7 B/voxel.
const LEVEL_BYTES_PER_VOXEL_PER_CHANNEL: u64 = 20;
/// Each channel keeps one smoothed moving image on the moving image's own grid for the whole
/// level (`syn.rs`, `syn_fields`); the per-voxel figures above count it on the fixed grid.
const MOVING_IMAGE_BYTES_PER_VOXEL: u64 = 4;

/// Environment override of the device memory budget, in mebibytes.
pub const MEMORY_BUDGET_ENV: &str = "FASTWARP_GPU_MEMORY_BUDGET_MB";
pub const VIEWER_RESERVE_BYTES: u64 = 1 << 30;
pub const FALLBACK_MEMORY_CAP_BYTES: u64 = 4 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum MemoryFigure {
    DxgiLocalBudget,
    VulkanDeviceLocalHeap,
    VulkanHeapBudget,
    MetalRecommendedWorkingSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct DeviceMemory {
    pub bytes: u64,
    pub figure: MemoryFigure,
}

/// One memory heap as `VK_EXT_memory_budget` reports it.
#[cfg(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd"
))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeapBudget {
    pub device_local: bool,
    pub size: u64,
    pub budget: u64,
    pub usage: u64,
}

/// The most a device-local heap has left within its budget (`budget` capped at the heap size,
/// minus `usage`), or `None` when no device-local heap reports a budget.
#[cfg(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd"
))]
pub(crate) fn heap_budget_bytes(heaps: &[HeapBudget]) -> Option<u64> {
    heaps
        .iter()
        .filter(|h| h.device_local && h.budget > 0 && h.size > 0)
        .map(|h| h.budget.min(h.size).saturating_sub(h.usage))
        .max()
}

/// The Vulkan memory figure: the heap budget when `heaps` has one, else `heap_size`.
#[cfg(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd"
))]
pub(crate) fn vulkan_figure(
    heaps: Option<&[HeapBudget]>,
    heap_size: Option<DeviceMemory>,
) -> Option<DeviceMemory> {
    match heaps.and_then(heap_budget_bytes) {
        Some(bytes) => Some(DeviceMemory {
            bytes,
            figure: MemoryFigure::VulkanHeapBudget,
        }),
        None => heap_size,
    }
}

/// The Metal memory figure: the device's `recommendedMaxWorkingSetSize` minus its
/// `currentAllocatedSize`, or `None` when the device recommends nothing.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn metal_figure(recommended: u64, allocated: u64) -> Option<DeviceMemory> {
    (recommended > 0).then_some(DeviceMemory {
        bytes: recommended.saturating_sub(allocated),
        figure: MemoryFigure::MetalRecommendedWorkingSet,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetSource {
    Env,
    Device(MemoryFigure),
    Fallback,
}

pub fn memory_budget(
    env_mb: Option<&str>,
    device: Option<DeviceMemory>,
    shares_host_memory: bool,
    max_buffer_size: u64,
) -> (u64, BudgetSource) {
    if let Some(mb) = env_mb.and_then(|v| v.trim().parse::<u64>().ok()) {
        return (mb.saturating_mul(1024 * 1024), BudgetSource::Env);
    }
    let (usable, source) = match device {
        Some(m) if shares_host_memory => (m.bytes / 2, BudgetSource::Device(m.figure)),
        Some(m) => (m.bytes, BudgetSource::Device(m.figure)),
        None => (
            max_buffer_size.min(FALLBACK_MEMORY_CAP_BYTES),
            BudgetSource::Fallback,
        ),
    };
    (usable.saturating_sub(VIEWER_RESERVE_BYTES), source)
}

/// The device limits a capacity check needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityCaps {
    /// Largest single storage binding the device permits.
    pub max_storage_buffer_binding_size: u64,
    /// Total device allocation the finest level may use.
    pub memory_budget: u64,
}

impl CapacityCaps {
    pub fn from_context(ctx: &GpuContext) -> Self {
        let shares = matches!(ctx.info.device_type.as_str(), "IntegratedGpu" | "Cpu");
        Self::build(&ctx.caps, ctx.memory, shares)
    }

    /// Reads the binding limit from `caps` and the budget from [`MEMORY_BUDGET_ENV`] if set,
    /// else `caps.max_buffer_size` capped at [`FALLBACK_MEMORY_CAP_BYTES`] minus
    /// [`VIEWER_RESERVE_BYTES`] (no device-memory figure is available here; the fallback is noted
    /// once on stderr because it is not a heap figure).
    pub fn from_gpu_caps(caps: &GpuCaps) -> Self {
        Self::build(caps, None, false)
    }

    fn build(caps: &GpuCaps, device: Option<DeviceMemory>, shares_host_memory: bool) -> Self {
        let env = crate::device::env_var(MEMORY_BUDGET_ENV);
        let (budget, source) = memory_budget(
            env.as_deref(),
            device,
            shares_host_memory,
            caps.max_buffer_size,
        );
        if source == BudgetSource::Fallback {
            FALLBACK_BUDGET_NOTE.call_once(|| {
                eprintln!(
                    "fastwarp: note: the graphics API reports no device memory and \
                     {MEMORY_BUDGET_ENV} is unset; the GPU memory budget is min(max_buffer_size, \
                     {FALLBACK_MEMORY_CAP_BYTES}) minus {VIEWER_RESERVE_BYTES} bytes kept for the \
                     viewers = {budget} bytes (set {MEMORY_BUDGET_ENV} to the device heap)"
                );
            });
        }
        Self {
            max_storage_buffer_binding_size: caps.max_storage_buffer_binding_size,
            memory_budget: budget,
        }
    }
}

/// What a grid needs and what the device allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityPlan {
    /// Voxels of the finest registration level.
    pub voxels: u64,
    /// Bytes of the largest single storage binding (the three-plane displacement field, or a
    /// moving image with more than three times the voxels).
    pub largest_binding_bytes: u64,
    /// Bytes of the finest level's total device allocation.
    pub level_total_bytes: u64,
    /// Binding limit the plan was checked against.
    pub binding_limit: u64,
    /// Memory budget the plan was checked against.
    pub memory_budget: u64,
}

/// Why a grid cannot fit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapacityError {
    /// The largest binding (the three-plane displacement field, or a larger moving image)
    /// exceeds `max_storage_buffer_binding_size`.
    #[error(
        "the finest registration level {dims:?} needs a storage binding of {need} bytes \
         (the three-plane displacement field, or the moving image when that is larger), but the \
         device binds at most {limit} bytes per storage buffer"
    )]
    Binding {
        /// Grid dimensions of the finest level.
        dims: [usize; 3],
        /// Bytes the binding needs.
        need: u64,
        /// Limit the device permits.
        limit: u64,
    },
    /// The finest level's total device allocation exceeds the memory budget.
    #[error(
        "the finest registration level {dims:?} needs {need} bytes of device memory, \
         but the budget is {budget} bytes"
    )]
    Memory {
        /// Grid dimensions of the finest level.
        dims: [usize; 3],
        /// Bytes the level needs.
        need: u64,
        /// Budget the device permits.
        budget: u64,
    },
}

/// Checks whether `grid_dims` at `channels` channels fits `caps`, for a moving image on a grid of
/// the same size. Pure; no device is touched.
pub fn plan_capacity(
    grid_dims: [usize; 3],
    channels: usize,
    caps: &CapacityCaps,
) -> Result<CapacityPlan, CapacityError> {
    plan_capacity_pair(grid_dims, grid_dims, channels, caps)
}

/// Like [`plan_capacity`] for a moving image whose registration grid is `moving_dims` (the moving
/// image resampled on its own, not onto the fixed grid). A moving grid with more voxels than the
/// fixed one adds its excess for every channel; a smaller one is planned as if it were the size
/// of the fixed grid.
pub fn plan_capacity_pair(
    grid_dims: [usize; 3],
    moving_dims: [usize; 3],
    channels: usize,
    caps: &CapacityCaps,
) -> Result<CapacityPlan, CapacityError> {
    let count = |dims: [usize; 3]| {
        dims.iter()
            .fold(1u64, |acc, d| acc.saturating_mul(*d as u64))
    };
    let voxels = count(grid_dims);
    let moving_voxels = count(moving_dims);
    let channels = channels.max(1) as u64;
    let largest_binding_bytes = voxels
        .saturating_mul(LARGEST_BINDING_BYTES_PER_VOXEL)
        .max(moving_voxels.saturating_mul(MOVING_IMAGE_BYTES_PER_VOXEL));
    let per_voxel = LEVEL_BYTES_PER_VOXEL_TWO_CHANNELS - LEVEL_BYTES_PER_VOXEL_PER_CHANNEL
        + LEVEL_BYTES_PER_VOXEL_PER_CHANNEL * (channels - 1);
    let moving_excess = moving_voxels
        .saturating_sub(voxels)
        .saturating_mul(MOVING_IMAGE_BYTES_PER_VOXEL)
        .saturating_mul(channels);
    let level_total_bytes = voxels
        .saturating_mul(per_voxel)
        .saturating_add(moving_excess);
    if largest_binding_bytes > caps.max_storage_buffer_binding_size {
        return Err(CapacityError::Binding {
            dims: grid_dims,
            need: largest_binding_bytes,
            limit: caps.max_storage_buffer_binding_size,
        });
    }
    if level_total_bytes > caps.memory_budget {
        return Err(CapacityError::Memory {
            dims: grid_dims,
            need: level_total_bytes,
            budget: caps.memory_budget,
        });
    }
    Ok(CapacityPlan {
        voxels,
        largest_binding_bytes,
        level_total_bytes,
        binding_limit: caps.max_storage_buffer_binding_size,
        memory_budget: caps.memory_budget,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn arc() -> CapacityCaps {
        CapacityCaps {
            max_storage_buffer_binding_size: 1_073_741_820,
            memory_budget: 17 * GIB,
        }
    }

    fn caps(max_buffer_size: u64) -> GpuCaps {
        GpuCaps {
            max_storage_buffer_binding_size: 1_073_741_820,
            max_buffer_size,
            max_uniform_buffer_binding_size: 65_536,
            max_compute_workgroup_storage_size: 32_768,
            max_compute_invocations_per_workgroup: 1024,
            max_compute_workgroup_size: [1024, 1024, 64],
            max_compute_workgroups_per_dimension: 65_535,
            max_storage_buffers_per_shader_stage: 16,
            max_bind_groups: 4,
            max_bindings_per_bind_group: 1000,
            max_immediate_size: 0,
            timestamp_query: true,
            timestamp_query_inside_encoders: true,
            subgroup: true,
            immediates: false,
            shader_f16: false,
            shader_float32_atomic: false,
            float32_filterable: false,
            timestamp_period_ns: None,
            feature_names: Vec::new(),
        }
    }

    #[test]
    fn budget_defaults_to_the_capped_buffer_proxy_minus_the_reserve_and_the_env_overrides_it() {
        std::env::remove_var("DBSLOC_GPU_MEMORY_BUDGET_MB");
        std::env::remove_var(MEMORY_BUDGET_ENV);
        let small = caps(2 * GIB);
        assert_eq!(CapacityCaps::from_gpu_caps(&small).memory_budget, GIB);
        let caps = caps(64 * GIB);
        assert_eq!(CapacityCaps::from_gpu_caps(&caps).memory_budget, 3 * GIB);
        std::env::set_var(MEMORY_BUDGET_ENV, " 128 ");
        assert_eq!(
            CapacityCaps::from_gpu_caps(&caps).memory_budget,
            128 * 1024 * 1024
        );
        std::env::set_var(MEMORY_BUDGET_ENV, "not-a-number");
        assert_eq!(CapacityCaps::from_gpu_caps(&caps).memory_budget, 3 * GIB);
        std::env::set_var(MEMORY_BUDGET_ENV, "16");
        let from_env = CapacityCaps::from_gpu_caps(&caps);
        assert_eq!(from_env.memory_budget, 16 * 1024 * 1024);
        assert_eq!(
            from_env.max_storage_buffer_binding_size,
            caps.max_storage_buffer_binding_size
        );
        std::env::remove_var(MEMORY_BUDGET_ENV);
    }

    fn vulkan(bytes: u64) -> Option<DeviceMemory> {
        Some(DeviceMemory {
            bytes,
            figure: MemoryFigure::VulkanDeviceLocalHeap,
        })
    }

    #[cfg(any(
        windows,
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd"
    ))]
    fn heap(device_local: bool, size: u64, budget: u64, usage: u64) -> HeapBudget {
        HeapBudget {
            device_local,
            size,
            budget,
            usage,
        }
    }

    #[cfg(any(
        windows,
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd"
    ))]
    #[test]
    fn the_heap_budget_is_the_largest_device_local_budget_left_after_the_usage() {
        let heaps = [
            heap(false, 32 * GIB, 30 * GIB, 0),
            heap(true, 8 * GIB, 7 * GIB, 2 * GIB),
            heap(true, 256 << 20, 200 << 20, 0),
        ];
        assert_eq!(heap_budget_bytes(&heaps), Some(5 * GIB));
        assert_eq!(
            heap_budget_bytes(&[heap(true, 8 * GIB, 9 * GIB, GIB)]),
            Some(7 * GIB)
        );
        assert_eq!(
            heap_budget_bytes(&[heap(true, 8 * GIB, 7 * GIB, 8 * GIB)]),
            Some(0)
        );
    }

    #[cfg(any(
        windows,
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd"
    ))]
    #[test]
    fn without_a_device_local_heap_reporting_a_budget_there_is_no_heap_budget() {
        assert_eq!(heap_budget_bytes(&[]), None);
        assert_eq!(
            heap_budget_bytes(&[heap(false, 32 * GIB, 30 * GIB, 0)]),
            None
        );
        assert_eq!(heap_budget_bytes(&[heap(true, 8 * GIB, 0, 0)]), None);
        assert_eq!(heap_budget_bytes(&[heap(true, 0, 7 * GIB, 0)]), None);
    }

    #[cfg(any(
        windows,
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd"
    ))]
    #[test]
    fn the_vulkan_figure_falls_back_to_the_heap_size_without_a_budget() {
        let size = vulkan(8 * GIB);
        let heaps = [heap(true, 8 * GIB, 7 * GIB, GIB)];
        assert_eq!(
            vulkan_figure(Some(&heaps), size),
            Some(DeviceMemory {
                bytes: 6 * GIB,
                figure: MemoryFigure::VulkanHeapBudget
            })
        );
        assert_eq!(vulkan_figure(None, size), size);
        assert_eq!(vulkan_figure(Some(&[]), size), size);
        assert_eq!(
            vulkan_figure(Some(&[heap(true, 8 * GIB, 0, 0)]), size),
            size
        );
        assert_eq!(vulkan_figure(None, None), None);
    }

    #[test]
    fn the_metal_figure_is_the_recommended_working_set_left_after_the_allocations() {
        let metal = |bytes| {
            Some(DeviceMemory {
                bytes,
                figure: MemoryFigure::MetalRecommendedWorkingSet,
            })
        };
        assert_eq!(metal_figure(12 * GIB, GIB), metal(11 * GIB));
        assert_eq!(metal_figure(12 * GIB, 0), metal(12 * GIB));
        assert_eq!(metal_figure(GIB, 2 * GIB), metal(0));
        assert_eq!(metal_figure(0, 0), None);
    }

    #[test]
    fn a_discrete_device_keeps_its_reported_memory_minus_the_viewer_reserve() {
        assert_eq!(
            memory_budget(None, vulkan(8 * GIB), false, 64 * GIB),
            (
                7 * GIB,
                BudgetSource::Device(MemoryFigure::VulkanDeviceLocalHeap)
            )
        );
        let dxgi = Some(DeviceMemory {
            bytes: 7 * GIB,
            figure: MemoryFigure::DxgiLocalBudget,
        });
        assert_eq!(
            memory_budget(None, dxgi, false, GIB),
            (6 * GIB, BudgetSource::Device(MemoryFigure::DxgiLocalBudget))
        );
    }

    #[test]
    fn a_device_sharing_host_memory_gets_half_its_reported_memory_minus_the_reserve() {
        assert_eq!(
            memory_budget(None, vulkan(18 * GIB), true, 64 * GIB).0,
            8 * GIB
        );
        assert_eq!(memory_budget(None, vulkan(GIB), true, 64 * GIB).0, 0);
    }

    #[test]
    fn the_env_override_wins_verbatim_over_the_device_figure() {
        assert_eq!(
            memory_budget(Some(" 2048 "), vulkan(8 * GIB), true, 64 * GIB),
            (2 * GIB, BudgetSource::Env)
        );
        assert_eq!(
            memory_budget(Some("lots"), vulkan(8 * GIB), false, 64 * GIB),
            (
                7 * GIB,
                BudgetSource::Device(MemoryFigure::VulkanDeviceLocalHeap)
            )
        );
    }

    #[test]
    fn without_a_figure_the_buffer_proxy_is_capped_and_the_reserve_saturates() {
        assert_eq!(
            memory_budget(None, None, false, 64 * GIB),
            (
                FALLBACK_MEMORY_CAP_BYTES - VIEWER_RESERVE_BYTES,
                BudgetSource::Fallback
            )
        );
        assert_eq!(memory_budget(None, None, true, 2 * GIB).0, GIB);
        assert_eq!(memory_budget(None, None, false, 256 << 20).0, 0);
    }

    #[test]
    fn the_shared_arc_heap_rejects_point_five_mm_and_fits_point_eight_mm() {
        let caps = CapacityCaps {
            memory_budget: memory_budget(None, vulkan(18 * GIB), true, 64 * GIB).0,
            ..arc()
        };
        assert!(matches!(
            plan_capacity([394, 466, 378], 2, &caps),
            Err(CapacityError::Memory { .. })
        ));
        assert!(plan_capacity([246, 291, 236], 2, &caps).is_ok());
        assert!(plan_capacity([197, 233, 189], 2, &caps).is_ok());
    }

    #[test]
    fn one_mm_fits_on_the_arc_binding() {
        let plan = plan_capacity([197, 233, 189], 2, &arc()).expect("1.0 mm must fit");
        assert!(plan.largest_binding_bytes < GIB, "{plan:?}");
        assert_eq!(plan.level_total_bytes, 197 * 233 * 189 * 188);
        assert!(plan.level_total_bytes < 17 * GIB, "{plan:?}");
        let one = plan_capacity([197, 233, 189], 1, &arc()).expect("one channel must fit");
        assert_eq!(one.level_total_bytes, 197 * 233 * 189 * 168);
    }

    #[test]
    fn every_channel_past_the_first_adds_the_same_bytes() {
        let caps = CapacityCaps {
            max_storage_buffer_binding_size: u64::MAX,
            memory_budget: u64::MAX,
        };
        let per_voxel = |channels| {
            plan_capacity([10, 10, 10], channels, &caps)
                .unwrap()
                .level_total_bytes
                / 1000
        };
        assert_eq!(per_voxel(1), 168);
        assert_eq!(per_voxel(2), 188);
        assert_eq!(per_voxel(3), 208);
        assert_eq!(per_voxel(0), 168);
    }

    #[test]
    fn point_five_mm_fits_the_split_layout() {
        let plan = plan_capacity([394, 466, 378], 2, &arc()).expect("0.5 mm must fit");
        assert_eq!(plan.largest_binding_bytes, 394 * 466 * 378 * 12);
        assert!(plan.largest_binding_bytes < GIB, "{plan:?}");
        assert_eq!(plan.level_total_bytes, 394 * 466 * 378 * 188);
        assert!(plan.level_total_bytes < 17 * GIB, "{plan:?}");
    }

    #[test]
    fn the_split_layout_fits_a_binding_the_packed_planes_would_not() {
        let caps = CapacityCaps {
            max_storage_buffer_binding_size: 15_000_000,
            memory_budget: u64::MAX,
        };
        let plan = plan_capacity([100, 100, 100], 2, &caps).expect("split layout must fit");
        assert_eq!(plan.largest_binding_bytes, 12_000_000);
        assert!(plan.largest_binding_bytes <= caps.max_storage_buffer_binding_size);
        assert!(1_000_000u64 * 20 > caps.max_storage_buffer_binding_size);
    }

    #[test]
    fn the_boundary_is_near_the_field_binding_limit() {
        let caps = CapacityCaps {
            memory_budget: u64::MAX,
            ..arc()
        };
        assert!(plan_capacity([447, 447, 447], 2, &caps).is_ok());
        assert!(matches!(
            plan_capacity([448, 447, 447], 2, &caps),
            Err(CapacityError::Binding { .. })
        ));
    }

    #[test]
    fn a_tiny_grid_fits_a_tiny_budget_and_a_big_one_fails_it() {
        let caps = CapacityCaps {
            max_storage_buffer_binding_size: 1 << 30,
            memory_budget: 1 << 20,
        };
        assert!(plan_capacity([4, 4, 4], 1, &caps).is_ok());
        assert!(matches!(
            plan_capacity([256, 256, 256], 1, &caps),
            Err(CapacityError::Memory { .. })
        ));
    }

    fn unlimited() -> CapacityCaps {
        CapacityCaps {
            max_storage_buffer_binding_size: u64::MAX,
            memory_budget: u64::MAX,
        }
    }

    #[test]
    fn a_moving_grid_of_the_fixed_size_or_smaller_plans_like_the_fixed_grid_alone() {
        for channels in [1, 2] {
            let alone = plan_capacity([197, 233, 189], channels, &arc()).unwrap();
            let same =
                plan_capacity_pair([197, 233, 189], [197, 233, 189], channels, &arc()).unwrap();
            let smaller =
                plan_capacity_pair([197, 233, 189], [100, 120, 90], channels, &arc()).unwrap();
            assert_eq!(same, alone);
            assert_eq!(smaller, alone);
        }
    }

    #[test]
    fn a_larger_moving_grid_adds_its_excess_image_bytes_for_every_channel() {
        let fixed = 10 * 10 * 10;
        let moving = 20 * 20 * 20;
        for channels in [1u64, 2] {
            let plan =
                plan_capacity_pair([10, 10, 10], [20, 20, 20], channels as usize, &unlimited())
                    .unwrap();
            let alone = plan_capacity([10, 10, 10], channels as usize, &unlimited()).unwrap();
            assert_eq!(plan.voxels, fixed);
            assert_eq!(
                plan.level_total_bytes,
                alone.level_total_bytes + (moving - fixed) * 4 * channels
            );
        }
    }

    #[test]
    fn a_larger_moving_grid_can_exceed_a_budget_the_fixed_grid_alone_fits() {
        let fixed = [128, 128, 128];
        let moving = [256, 256, 256];
        let need = plan_capacity(fixed, 2, &unlimited())
            .unwrap()
            .level_total_bytes;
        let caps = CapacityCaps {
            max_storage_buffer_binding_size: u64::MAX,
            memory_budget: need,
        };
        assert!(plan_capacity(fixed, 2, &caps).is_ok());
        assert!(matches!(
            plan_capacity_pair(fixed, moving, 2, &caps),
            Err(CapacityError::Memory { .. })
        ));
    }

    #[test]
    fn a_moving_image_with_over_three_times_the_voxels_is_the_largest_binding() {
        let plan = plan_capacity_pair([100, 100, 100], [200, 200, 100], 1, &unlimited()).unwrap();
        assert_eq!(plan.largest_binding_bytes, 200 * 200 * 100 * 4);
        let caps = CapacityCaps {
            max_storage_buffer_binding_size: 100 * 100 * 100 * 12,
            memory_budget: u64::MAX,
        };
        assert!(plan_capacity([100, 100, 100], 1, &caps).is_ok());
        assert!(matches!(
            plan_capacity_pair([100, 100, 100], [200, 200, 100], 1, &caps),
            Err(CapacityError::Binding { .. })
        ));
    }
}
