// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Adapter selection shared by every interactive window (the application, `dbs-localizer scene`,
//! `dbs-localizer checkreg`): a hardware adapter that can present to the window first, the operating
//! system's software rasteriser (WARP on Windows, lavapipe / llvmpipe on Linux) when none can, so
//! a machine without a usable graphics driver still opens the window. `DBSLOC_FORCE_SOFTWARE_RENDERING=1`
//! puts the software adapter first; it never touches the registration engine, which keeps its own
//! selection in `fastwarp-gpu`, which reads `FASTWARP_GPU_*` and still accepts `DBSLOC_GPU_*`.

use std::sync::Arc;

use eframe::egui_wgpu::{SurfaceConfig, WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
use wgpu::{Adapter, AdapterInfo, Backend, DeviceType, Limits, Surface};

/// Set to `1` / `true` / `yes` / `on` to draw on the software rasteriser even when a graphics card works.
pub const FORCE_SOFTWARE_ENV: &str = "DBSLOC_FORCE_SOFTWARE_RENDERING";

/// Whether an environment value opts in (`1`, `true`, `yes`, `on`; case-insensitive, trimmed).
pub fn env_is_truthy(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// `DBSLOC_FORCE_SOFTWARE_RENDERING` is set.
pub fn force_software_rendering() -> bool {
    env_is_truthy(std::env::var(FORCE_SOFTWARE_ENV).ok().as_deref())
}

/// A software rasteriser (WARP, lavapipe, SwiftShader) rather than a graphics card.
pub fn is_software(device_type: DeviceType) -> bool {
    device_type == DeviceType::Cpu
}

/// What a window draws on, for the status line and the driver notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderAdapter {
    pub name: String,
    /// Graphics API (`Vulkan`, `Dx12`, `Metal`, `Gl`).
    pub backend: String,
    /// `DiscreteGpu`, `IntegratedGpu`, `VirtualGpu`, `Cpu` or `Other`.
    pub device_type: String,
    pub driver: String,
    pub software: bool,
}

impl RenderAdapter {
    pub fn from_info(info: &AdapterInfo) -> Self {
        Self {
            name: info.name.clone(),
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: format!("{} {}", info.driver, info.driver_info)
                .trim()
                .to_owned(),
            software: is_software(info.device_type),
        }
    }

    /// `Name (Backend, DeviceType)`.
    pub fn label(&self) -> String {
        format!("{} ({}, {})", self.name, self.backend, self.device_type)
    }
}

/// Sort key of an adapter: lower is picked first. Hardware classes in the order discrete,
/// integrated, virtual, other, software; with `force_software` the software rasteriser moves to
/// the front and the hardware order is kept behind it. Within a class the API order is the one
/// `wgpu` enumerates today (Vulkan / Metal, then Direct3D 12, then OpenGL), so a machine that
/// works now keeps its adapter.
pub fn adapter_rank(device_type: DeviceType, backend: Backend, force_software: bool) -> (u8, u8) {
    let class = match device_type {
        DeviceType::DiscreteGpu => 0,
        DeviceType::IntegratedGpu => 1,
        DeviceType::VirtualGpu => 2,
        DeviceType::Other => 3,
        DeviceType::Cpu => 4,
    };
    let class = match (force_software, device_type) {
        (true, DeviceType::Cpu) => 0,
        (true, _) => class + 1,
        (false, _) => class,
    };
    let api = match backend {
        Backend::Vulkan | Backend::Metal => 0,
        Backend::Dx12 => 1,
        Backend::Gl => 2,
        Backend::BrowserWebGpu => 3,
        Backend::Noop => 4,
    };
    (class, api)
}

fn presents_to(adapter: &Adapter, surface: Option<&Surface<'_>>) -> bool {
    surface.is_none_or(|s| !s.get_capabilities(adapter).formats.is_empty())
}

/// The adapter a window draws on: the best-ranked one that can present to `surface`.
pub fn select_adapter(
    adapters: &[Adapter],
    surface: Option<&Surface<'_>>,
    force_software: bool,
) -> Result<Adapter, String> {
    let mut ranked: Vec<((u8, u8), usize)> = adapters
        .iter()
        .enumerate()
        .filter(|(_, a)| presents_to(a, surface))
        .map(|(i, a)| {
            let info = a.get_info();
            (
                adapter_rank(info.device_type, info.backend, force_software),
                i,
            )
        })
        .collect();
    ranked.sort();
    match ranked.first() {
        Some((_, i)) => {
            let chosen = adapters[*i].clone();
            let info = chosen.get_info();
            if is_software(info.device_type) {
                let why = if force_software {
                    format!("{FORCE_SOFTWARE_ENV} is set")
                } else {
                    "no graphics card can draw this window".to_owned()
                };
                eprintln!(
                    "dbsloc: {why}; rendering on the software adapter {}",
                    info.name
                );
            }
            Ok(chosen)
        }
        None => Err(no_adapter_message(adapters)),
    }
}

fn no_adapter_message(adapters: &[Adapter]) -> String {
    if adapters.is_empty() {
        return "no graphics adapter was found (no graphics driver, and no software renderer)"
            .to_owned();
    }
    let names: Vec<String> = adapters
        .iter()
        .map(|a| RenderAdapter::from_info(&a.get_info()).label())
        .collect();
    format!(
        "none of the graphics adapters can draw to the window: {}",
        names.join("; ")
    )
}

/// The device limits every window asks for, lowered to what the adapter offers so a software
/// rasteriser with smaller limits still gets a device rather than a failed request.
pub fn device_limits(adapter_limits: &Limits, backend: Backend) -> Limits {
    let base = if backend == Backend::Gl {
        Limits::downlevel_webgl2_defaults()
    } else {
        Limits::default()
    };
    Limits {
        max_texture_dimension_2d: 8192,
        ..base
    }
    .or_worse_values_from(adapter_limits)
}

fn device_descriptor(adapter: &Adapter) -> wgpu::DeviceDescriptor<'static> {
    wgpu::DeviceDescriptor {
        label: Some("dbsloc wgpu device"),
        required_limits: device_limits(&adapter.limits(), adapter.get_info().backend),
        ..Default::default()
    }
}

/// The `eframe` wgpu configuration of every interactive window: one frame of latency, the
/// adapter from [`select_adapter`] and the limits from [`device_limits`].
pub fn native_wgpu_configuration() -> WgpuConfiguration {
    let force = force_software_rendering();
    let mut create = WgpuSetupCreateNew::without_display_handle();
    create.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
        select_adapter(adapters, surface, force)
    }));
    create.device_descriptor = Arc::new(device_descriptor);
    WgpuConfiguration {
        surface: SurfaceConfig::LOW_LATENCY,
        wgpu_setup: WgpuSetup::CreateNew(create),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_beats_software_unless_forced() {
        let r = |t, b| adapter_rank(t, b, false);
        assert!(
            r(DeviceType::DiscreteGpu, Backend::Vulkan)
                < r(DeviceType::IntegratedGpu, Backend::Vulkan)
        );
        assert!(
            r(DeviceType::IntegratedGpu, Backend::Dx12)
                < r(DeviceType::VirtualGpu, Backend::Vulkan)
        );
        assert!(r(DeviceType::VirtualGpu, Backend::Gl) < r(DeviceType::Other, Backend::Vulkan));
        assert!(r(DeviceType::Other, Backend::Gl) < r(DeviceType::Cpu, Backend::Dx12));
        assert!(
            r(DeviceType::DiscreteGpu, Backend::Vulkan) < r(DeviceType::DiscreteGpu, Backend::Dx12)
        );
        assert_eq!(
            r(DeviceType::DiscreteGpu, Backend::Vulkan),
            r(DeviceType::DiscreteGpu, Backend::Metal)
        );
        let f = |t, b| adapter_rank(t, b, true);
        assert!(f(DeviceType::Cpu, Backend::Dx12) < f(DeviceType::DiscreteGpu, Backend::Vulkan));
        assert!(
            f(DeviceType::DiscreteGpu, Backend::Vulkan)
                < f(DeviceType::IntegratedGpu, Backend::Vulkan)
        );
    }

    #[test]
    fn the_force_variable_accepts_the_usual_spellings() {
        for v in ["1", "true", " YES ", "on"] {
            assert!(env_is_truthy(Some(v)), "{v}");
        }
        for v in ["0", "false", "", "no", "off"] {
            assert!(!env_is_truthy(Some(v)), "{v}");
        }
        assert!(!env_is_truthy(None));
    }

    #[test]
    fn limits_never_exceed_the_adapter() {
        let small = Limits {
            max_texture_dimension_2d: 4096,
            max_storage_buffer_binding_size: 1 << 20,
            min_uniform_buffer_offset_alignment: 1024,
            ..Limits::downlevel_defaults()
        };
        let l = device_limits(&small, Backend::Dx12);
        assert_eq!(l.max_texture_dimension_2d, 4096);
        assert_eq!(l.max_storage_buffer_binding_size, 1 << 20);
        assert_eq!(l.min_uniform_buffer_offset_alignment, 1024);
        assert!(l.check_limits(&small));
        let big = Limits {
            max_texture_dimension_2d: 16384,
            ..Limits::default()
        };
        assert_eq!(
            device_limits(&big, Backend::Vulkan).max_texture_dimension_2d,
            8192
        );
        assert!(device_limits(&big, Backend::Gl).check_limits(&big));
    }

    #[test]
    fn the_configuration_keeps_one_frame_of_latency_and_a_selector() {
        let c = native_wgpu_configuration();
        assert_eq!(c.surface.desired_maximum_frame_latency, Some(1));
        match c.wgpu_setup {
            WgpuSetup::CreateNew(create) => assert!(create.native_adapter_selector.is_some()),
            WgpuSetup::Existing(_) => panic!("expected CreateNew"),
        }
    }

    #[test]
    fn software_status_follows_the_device_type() {
        assert!(is_software(DeviceType::Cpu));
        assert!(!is_software(DeviceType::IntegratedGpu));
        let info = AdapterInfo {
            name: "Microsoft Basic Render Driver".to_owned(),
            vendor: 0x1414,
            device: 0x8c,
            device_type: DeviceType::Cpu,
            device_pci_bus_id: String::new(),
            driver: "D3D12".to_owned(),
            driver_info: String::new(),
            backend: Backend::Dx12,
            subgroup_min_size: 4,
            subgroup_max_size: 128,
            transient_saves_memory: Some(false),
            limit_bucket: None,
        };
        let r = RenderAdapter::from_info(&info);
        assert!(r.software);
        assert_eq!(r.driver, "D3D12");
        assert_eq!(r.label(), "Microsoft Basic Render Driver (Dx12, Cpu)");
    }
}
