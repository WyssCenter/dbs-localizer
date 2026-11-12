// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::borrow::Cow;

use eframe::egui_wgpu::wgpu;
use glam::Vec3;

const COMPOSITE_SHADER: &str = include_str!("shaders/oit_composite.wgsl");

pub(crate) const OPAQUE_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;
pub(crate) const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub(crate) const REVEAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;

const ADD: wgpu::BlendComponent = wgpu::BlendComponent {
    src_factor: wgpu::BlendFactor::One,
    dst_factor: wgpu::BlendFactor::One,
    operation: wgpu::BlendOperation::Add,
};
const MULTIPLY_BY_ONE_MINUS_SRC: wgpu::BlendComponent = wgpu::BlendComponent {
    src_factor: wgpu::BlendFactor::Zero,
    dst_factor: wgpu::BlendFactor::OneMinusSrc,
    operation: wgpu::BlendOperation::Add,
};

pub(crate) fn accumulation_targets() -> [Option<wgpu::ColorTargetState>; 2] {
    [
        Some(wgpu::ColorTargetState {
            format: ACCUM_FORMAT,
            blend: Some(wgpu::BlendState {
                color: ADD,
                alpha: ADD,
            }),
            write_mask: wgpu::ColorWrites::ALL,
        }),
        Some(wgpu::ColorTargetState {
            format: REVEAL_FORMAT,
            blend: Some(wgpu::BlendState {
                color: MULTIPLY_BY_ONE_MINUS_SRC,
                alpha: MULTIPLY_BY_ONE_MINUS_SRC,
            }),
            write_mask: wgpu::ColorWrites::ALL,
        }),
    ]
}

pub(crate) fn depth_frame(points: impl IntoIterator<Item = Vec3>) -> [f32; 4] {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for p in points {
        if p.is_finite() {
            lo = lo.min(p);
            hi = hi.max(p);
        }
    }
    if lo.x > hi.x {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let c = 0.5 * (lo + hi);
    let r = (0.5 * (hi - lo).length()).max(1.0);
    [c.x, c.y, c.z, r]
}

pub(crate) struct SceneTargets {
    size: [u32; 2],
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
    accum: wgpu::TextureView,
    reveal: wgpu::TextureView,
    composite_bind: wgpu::BindGroup,
}

fn target_view(
    device: &wgpu::Device,
    label: &str,
    size: [u32; 2],
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

pub(crate) struct OitCompositor {
    color_format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    targets: Option<SceneTargets>,
}

impl OitCompositor {
    pub(crate) fn new(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        pass_depth_format: Option<wgpu::TextureFormat>,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene_oit_composite"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(COMPOSITE_SHADER)),
        });
        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                multisampled: false,
                view_dimension: wgpu::TextureViewDimension::D2,
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene_oit_composite"),
            entries: &[texture_entry(0), texture_entry(1), texture_entry(2)],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene_oit_composite_pl"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene_oit_composite"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_composite"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: pass_depth_format.map(|format| wgpu::DepthStencilState {
                format,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            color_format,
            layout,
            pipeline,
            targets: None,
        }
    }

    pub(crate) fn ensure_targets(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        let size = [size[0].max(1), size[1].max(1)];
        if self.targets.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let sampled = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let color = target_view(device, "scene_color", size, self.color_format, sampled);
        let depth = target_view(
            device,
            "scene_depth",
            size,
            OPAQUE_DEPTH_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let accum = target_view(device, "scene_oit_accum", size, ACCUM_FORMAT, sampled);
        let reveal = target_view(device, "scene_oit_reveal", size, REVEAL_FORMAT, sampled);
        let composite_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene_oit_composite_bg"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&color),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&accum),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&reveal),
                },
            ],
        });
        self.targets = Some(SceneTargets {
            size,
            color,
            depth,
            accum,
            reveal,
            composite_bind,
        });
    }

    pub(crate) fn targets(&self) -> Option<&SceneTargets> {
        self.targets.as_ref()
    }

    pub(crate) fn composite(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(t) = &self.targets else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &t.composite_bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

pub(crate) fn begin_opaque_pass<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    t: &SceneTargets,
) -> wgpu::RenderPass<'e> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("scene_opaque"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &t.color,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &t.depth,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(1.0),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        ..Default::default()
    })
}

pub(crate) fn begin_accumulation_pass<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    t: &SceneTargets,
) -> wgpu::RenderPass<'e> {
    let attachment = |view, clear| {
        Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(clear),
                store: wgpu::StoreOp::Store,
            },
        })
    };
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("scene_oit_accumulate"),
        color_attachments: &[
            attachment(&t.accum, wgpu::Color::TRANSPARENT),
            attachment(&t.reveal, wgpu::Color::WHITE),
        ],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &t.depth,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_frame_is_the_bounding_sphere_of_finite_points() {
        let pts = [
            Vec3::new(-10.0, 0.0, 0.0),
            Vec3::new(10.0, 4.0, -6.0),
            Vec3::new(f32::NAN, 1e9, 0.0),
        ];
        let [x, y, z, r] = depth_frame(pts);
        assert_eq!([x, y, z], [0.0, 2.0, -3.0]);
        assert!((r - 0.5 * (20.0_f32 * 20.0 + 16.0 + 36.0).sqrt()).abs() < 1e-5);
    }

    #[test]
    fn depth_frame_of_nothing_or_a_point_has_a_unit_radius() {
        assert_eq!(depth_frame([]), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(
            depth_frame([Vec3::new(1.0, 2.0, 3.0)]),
            [1.0, 2.0, 3.0, 1.0]
        );
    }

    #[test]
    fn accumulation_adds_and_revealage_multiplies() {
        let [accum, reveal] = accumulation_targets();
        let accum = accum.and_then(|t| t.blend.map(|b| (t.format, b)));
        let reveal = reveal.and_then(|t| t.blend.map(|b| (t.format, b)));
        let Some((accum_format, accum_blend)) = accum else {
            panic!("accumulation target without blending");
        };
        let Some((reveal_format, reveal_blend)) = reveal else {
            panic!("revealage target without blending");
        };
        assert_eq!(accum_format, ACCUM_FORMAT);
        assert_eq!(accum_blend.color, ADD);
        assert_eq!(accum_blend.alpha, ADD);
        assert_eq!(reveal_format, REVEAL_FORMAT);
        assert_eq!(reveal_blend.color.src_factor, wgpu::BlendFactor::Zero);
        assert_eq!(
            reveal_blend.color.dst_factor,
            wgpu::BlendFactor::OneMinusSrc
        );
    }

    #[test]
    fn accumulation_formats_are_blendable_without_optional_features() {
        let none = wgpu::Features::empty();
        for f in [ACCUM_FORMAT, REVEAL_FORMAT] {
            let caps = f.guaranteed_format_features(none);
            assert!(caps
                .flags
                .contains(wgpu::TextureFormatFeatureFlags::BLENDABLE));
            assert!(caps
                .allowed_usages
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT));
        }
    }
}
