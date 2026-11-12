// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use serde::Serialize;

/// Largest single buffer planned at 0.5 mm: the three-plane displacement field
/// (394 x 466 x 378 f32 planes), in bytes.
pub const LARGEST_PLANNED_BUFFER_BYTES: u64 = 394 * 466 * 378 * 4 * 3;
/// A storage binding of 256 MiB or less is a no-go (plan 8.4, probe 5).
pub const MIN_BINDING_BYTES_NO_GO: u64 = 256 * 1024 * 1024;
const MIN_WORKGROUP_STORAGE: u32 = 16 * 1024;
const MIN_STORAGE_BUFFERS: u32 = 8;
const MIN_INVOCATIONS: u32 = 256;

/// Outcome class of a go/no-go gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Verdict {
    Go,
    Borderline,
    NoGo,
}

impl Verdict {
    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Go => "GO",
            Verdict::Borderline => "BORDERLINE",
            Verdict::NoGo => "NO-GO",
        }
    }

    /// The worse of two verdicts.
    pub fn worst(self, other: Verdict) -> Verdict {
        match (self, other) {
            (Verdict::NoGo, _) | (_, Verdict::NoGo) => Verdict::NoGo,
            (Verdict::Borderline, _) | (_, Verdict::Borderline) => Verdict::Borderline,
            _ => Verdict::Go,
        }
    }
}

/// One requirement of plan section 2.3 checked against an adapter.
#[derive(Debug, Clone, Serialize)]
pub struct CapCheck {
    pub name: String,
    pub required: String,
    pub have: String,
    pub ok: bool,
}

/// Limits and features of an adapter relevant to the registration engine.
#[derive(Debug, Clone, Serialize)]
pub struct GpuCaps {
    pub max_storage_buffer_binding_size: u64,
    pub max_buffer_size: u64,
    pub max_uniform_buffer_binding_size: u64,
    pub max_compute_workgroup_storage_size: u32,
    pub max_compute_invocations_per_workgroup: u32,
    pub max_compute_workgroup_size: [u32; 3],
    pub max_compute_workgroups_per_dimension: u32,
    pub max_storage_buffers_per_shader_stage: u32,
    pub max_bind_groups: u32,
    pub max_bindings_per_bind_group: u32,
    pub max_immediate_size: u32,
    pub timestamp_query: bool,
    pub timestamp_query_inside_encoders: bool,
    pub subgroup: bool,
    pub immediates: bool,
    pub shader_f16: bool,
    pub shader_float32_atomic: bool,
    pub float32_filterable: bool,
    pub timestamp_period_ns: Option<f32>,
    pub feature_names: Vec<String>,
}

impl GpuCaps {
    /// Reads limits and features from an adapter.
    pub fn from_adapter(adapter: &wgpu::Adapter) -> Self {
        let l = adapter.limits();
        let f = adapter.features();
        GpuCaps {
            max_storage_buffer_binding_size: l.max_storage_buffer_binding_size,
            max_buffer_size: l.max_buffer_size,
            max_uniform_buffer_binding_size: l.max_uniform_buffer_binding_size,
            max_compute_workgroup_storage_size: l.max_compute_workgroup_storage_size,
            max_compute_invocations_per_workgroup: l.max_compute_invocations_per_workgroup,
            max_compute_workgroup_size: [
                l.max_compute_workgroup_size_x,
                l.max_compute_workgroup_size_y,
                l.max_compute_workgroup_size_z,
            ],
            max_compute_workgroups_per_dimension: l.max_compute_workgroups_per_dimension,
            max_storage_buffers_per_shader_stage: l.max_storage_buffers_per_shader_stage,
            max_bind_groups: l.max_bind_groups,
            max_bindings_per_bind_group: l.max_bindings_per_bind_group,
            max_immediate_size: l.max_immediate_size,
            timestamp_query: f.contains(wgpu::Features::TIMESTAMP_QUERY),
            timestamp_query_inside_encoders: f
                .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS),
            subgroup: f.contains(wgpu::Features::SUBGROUP),
            immediates: f.contains(wgpu::Features::IMMEDIATES),
            shader_f16: f.contains(wgpu::Features::SHADER_F16),
            shader_float32_atomic: f.contains(wgpu::Features::SHADER_FLOAT32_ATOMIC),
            float32_filterable: f.contains(wgpu::Features::FLOAT32_FILTERABLE),
            timestamp_period_ns: None,
            feature_names: f.iter_names().map(|(n, _)| n.to_string()).collect(),
        }
    }

    /// Requirements of plan section 2.3 (`probe()` pass criteria).
    pub fn requirements(&self, software: bool) -> Vec<CapCheck> {
        let mut v = Vec::new();
        v.push(CapCheck {
            name: "adapter is not a software rasteriser".into(),
            required: "device type != Cpu".into(),
            have: if software { "Cpu" } else { "hardware" }.into(),
            ok: !software,
        });
        v.push(CapCheck {
            name: "max_storage_buffer_binding_size".into(),
            required: format!(">= {LARGEST_PLANNED_BUFFER_BYTES} (0.5 mm displacement field)"),
            have: self.max_storage_buffer_binding_size.to_string(),
            ok: self.max_storage_buffer_binding_size >= LARGEST_PLANNED_BUFFER_BYTES,
        });
        v.push(CapCheck {
            name: "max_buffer_size".into(),
            required: format!(">= {LARGEST_PLANNED_BUFFER_BYTES}"),
            have: self.max_buffer_size.to_string(),
            ok: self.max_buffer_size >= LARGEST_PLANNED_BUFFER_BYTES,
        });
        v.push(CapCheck {
            name: "max_compute_workgroup_storage_size".into(),
            required: format!(">= {MIN_WORKGROUP_STORAGE}"),
            have: self.max_compute_workgroup_storage_size.to_string(),
            ok: self.max_compute_workgroup_storage_size >= MIN_WORKGROUP_STORAGE,
        });
        v.push(CapCheck {
            name: "max_storage_buffers_per_shader_stage".into(),
            required: format!(">= {MIN_STORAGE_BUFFERS} (plan binds up to 14)"),
            have: self.max_storage_buffers_per_shader_stage.to_string(),
            ok: self.max_storage_buffers_per_shader_stage >= MIN_STORAGE_BUFFERS,
        });
        v.push(CapCheck {
            name: "max_compute_invocations_per_workgroup".into(),
            required: format!(">= {MIN_INVOCATIONS}"),
            have: self.max_compute_invocations_per_workgroup.to_string(),
            ok: self.max_compute_invocations_per_workgroup >= MIN_INVOCATIONS,
        });
        v
    }

    /// Probe 5 verdict: no-go when a binding of 256 MiB or less is the maximum, borderline when
    /// another requirement fails or DX12 lacks subgroups, go otherwise.
    pub fn verdict(&self, backend: &str, software: bool) -> Verdict {
        if self.max_storage_buffer_binding_size <= MIN_BINDING_BYTES_NO_GO {
            return Verdict::NoGo;
        }
        let all_ok = self.requirements(software).iter().all(|c| c.ok);
        let dx12_without_subgroups = backend.eq_ignore_ascii_case("dx12") && !self.subgroup;
        if all_ok && !dx12_without_subgroups {
            Verdict::Go
        } else {
            Verdict::Borderline
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(binding: u64) -> GpuCaps {
        GpuCaps {
            max_storage_buffer_binding_size: binding,
            max_buffer_size: 1 << 32,
            max_uniform_buffer_binding_size: 65536,
            max_compute_workgroup_storage_size: 32768,
            max_compute_invocations_per_workgroup: 1024,
            max_compute_workgroup_size: [1024, 1024, 64],
            max_compute_workgroups_per_dimension: 65535,
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
    fn verdict_classes() {
        assert_eq!(sample(1 << 30).verdict("vulkan", false), Verdict::Go);
        assert_eq!(sample(128 << 20).verdict("vulkan", false), Verdict::NoGo);
        assert_eq!(
            sample(270_000_000).verdict("vulkan", false),
            Verdict::Borderline
        );
        let mut c = sample(1 << 30);
        c.subgroup = false;
        assert_eq!(c.verdict("dx12", false), Verdict::Borderline);
        assert_eq!(c.verdict("vulkan", false), Verdict::Go);
        assert_eq!(sample(1 << 30).verdict("vulkan", true), Verdict::Borderline);
    }
}
