#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use super::*;
use crate::gpu_setup::env_is_truthy;

const W: u32 = 64;
const H: u32 = 64;
const BACKGROUND: [f32; 4] = [0.2, 0.3, 0.4, 1.0];
const LEFT: (u32, u32) = (16, 32);
const RIGHT: (u32, u32) = (48, 32);

fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

fn env_flag(name: &str) -> bool {
    env_is_truthy(std::env::var(name).ok().as_deref())
}

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let options = wgpu::RequestAdapterOptions {
        force_fallback_adapter: env_flag("WGPU_FORCE_FALLBACK_ADAPTER"),
        ..Default::default()
    };
    let adapter = match block_on(instance.request_adapter(&options)) {
        Ok(a) => a,
        Err(e) => {
            assert!(
                !env_flag("DBSLOC_GPU_REQUIRE_ADAPTER"),
                "no wgpu adapter: {e}"
            );
            eprintln!("skipping the scene render test: no wgpu adapter ({e})");
            return None;
        }
    };
    let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("scene_oit_test"),
        required_limits: wgpu::Limits::default().or_worse_values_from(&adapter.limits()),
        ..Default::default()
    }))
    .expect("device on the selected adapter");
    Some((device, queue))
}

#[derive(Clone, Copy)]
struct Quad {
    z: f32,
    x: (f32, f32),
    colour: [f32; 4],
}

impl Quad {
    fn full(z: f32, colour: [f32; 4]) -> Self {
        Self {
            z,
            x: (-2.0, 2.0),
            colour,
        }
    }

    fn append(&self, verts: &mut Vec<MeshVert>, indices: &mut Vec<u32>) -> (u32, u32) {
        let base = verts.len() as u32;
        let first = indices.len() as u32;
        for (x, y) in [
            (self.x.0, -2.0),
            (self.x.1, -2.0),
            (self.x.1, 2.0),
            (self.x.0, 2.0),
        ] {
            verts.push(MeshVert {
                pos: [x, y, self.z],
                n: [0.0, 0.0, 1.0],
                c: self.colour,
            });
        }
        indices.extend([0, 1, 2, 0, 2, 3].map(|i| base + i));
        (first, 6)
    }
}

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    r: &mut SceneWgpuRenderer,
    atlas: &[Quad],
    opaque: &[Quad],
) -> Vec<[u8; 4]> {
    let mut verts = Vec::new();
    let mut indices = Vec::new();
    for q in atlas {
        q.append(&mut verts, &mut indices);
    }
    r.atlas_indices = 0..indices.len() as u32;
    r.oit_frame = depth_frame(verts.iter().map(|v| Vec3::from_array(v.pos)));
    r.electrode_draw_ranges = opaque
        .iter()
        .map(|q| q.append(&mut verts, &mut indices))
        .collect();
    let usage_v = wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST;
    let usage_i = wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST;
    write_or_grow_buffer(
        device,
        queue,
        &mut r.mesh_vb,
        &mut r.mesh_vb_cap,
        bytemuck::cast_slice(&verts),
        usage_v,
        "mesh_vb",
    );
    write_or_grow_buffer(
        device,
        queue,
        &mut r.mesh_ib,
        &mut r.mesh_ib_cap,
        bytemuck::cast_slice(&indices),
        usage_i,
        "mesh_ib",
    );
    present(device, queue, r)
}

fn present(device: &wgpu::Device, queue: &wgpu::Queue, r: &mut SceneWgpuRenderer) -> Vec<[u8; 4]> {
    let eye = Vec3::new(0.0, 0.0, 5.0);
    let vp = directx::orthographic(-1.0, 1.0, -1.0, 1.0, 0.1, 10.0)
        * look_at_mat4(eye, Vec3::ZERO, Vec3::Y);
    r.write_globals(queue, vp, eye, Vec3::NEG_Z, true, false, 1.0, 0.5);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    r.encode_scene(device, &mut encoder, [W, H], [false; 3]);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scene_oit_test_target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene_oit_test_host"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: BACKGROUND[0] as f64,
                        g: BACKGROUND[1] as f64,
                        b: BACKGROUND[2] as f64,
                        a: BACKGROUND[3] as f64,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        r.compositor.composite(&mut pass);
    }
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("scene_oit_test_readback"),
        size: u64::from(W * H * 4),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(W * 4),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    readback.map_async(wgpu::MapMode::Read, .., |res| {
        res.expect("map the readback buffer");
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    let bytes = readback
        .get_mapped_range(..)
        .expect("view the mapped readback buffer");
    bytes.as_chunks::<4>().0.to_vec()
}

fn pixel(img: &[[u8; 4]], (x, y): (u32, u32)) -> [f32; 3] {
    let p = img[(y * W + x) as usize];
    [p[0], p[1], p[2]].map(|v| f32::from(v) / 255.0)
}

fn assert_rgb_close(got: [f32; 3], want: [f32; 3], tol: f32) {
    for c in 0..3 {
        assert!(
            (got[c] - want[c]).abs() <= tol,
            "pixel {got:?}, expected {want:?} (tolerance {tol})"
        );
    }
}

fn over(c: [f32; 4], below: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| c[i] * c[3] + below[i] * (1.0 - c[3]))
}

fn rgb(c: [f32; 4]) -> [f32; 3] {
    [c[0], c[1], c[2]]
}

fn reference_weight(z: f32, alpha: f32, frame: [f32; 4]) -> f32 {
    let d = -(z - frame[2]);
    let u = (0.5 + 0.5 * d / frame[3]).clamp(0.0, 1.0);
    let near = 1.0 - u;
    alpha * (8.0 * near * near * near).clamp(1e-2, 8.0)
}

fn renderer(device: &wgpu::Device) -> SceneWgpuRenderer {
    SceneWgpuRenderer::new(device, wgpu::TextureFormat::Rgba8Unorm, None)
}

const TOL: f32 = 2.5 / 255.0;

#[test]
fn two_translucent_layers_composite_the_same_in_either_draw_order() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let mut r = renderer(&device);
    let front = Quad::full(0.3, [0.9, 0.1, 0.1, 0.5]);
    let back = Quad::full(-0.3, [0.1, 0.8, 0.2, 0.6]);
    let a = render(&device, &queue, &mut r, &[front, back], &[]);
    let b = render(&device, &queue, &mut r, &[back, front], &[]);
    let worst = a
        .iter()
        .zip(&b)
        .flat_map(|(p, q)| p.iter().zip(q).map(|(x, y)| x.abs_diff(*y)))
        .max()
        .unwrap_or(0);
    assert!(worst <= 1, "draw order changed a pixel by {worst} / 255");

    let frame = depth_frame(
        [front, back]
            .iter()
            .flat_map(|q| [Vec3::new(-2.0, -2.0, q.z), Vec3::new(2.0, 2.0, q.z)]),
    );
    let mut sum = [0.0_f32; 4];
    let mut reveal = 1.0_f32;
    for q in [front, back] {
        let w = reference_weight(q.z, q.colour[3], frame);
        for (s, c) in sum.iter_mut().zip(q.colour).take(3) {
            *s += c * q.colour[3] * w;
        }
        sum[3] += q.colour[3] * w;
        reveal *= 1.0 - q.colour[3];
    }
    let want: [f32; 3] =
        std::array::from_fn(|i| sum[i] / sum[3] * (1.0 - reveal) + BACKGROUND[i] * reveal);
    assert_rgb_close(pixel(&a, LEFT), want, TOL);
    assert_rgb_close(pixel(&a, RIGHT), want, TOL);

    let w_front = reference_weight(front.z, 1.0, frame);
    let w_back = reference_weight(back.z, 1.0, frame);
    assert!(w_front > w_back);
}

#[test]
fn a_single_layer_matches_ordinary_alpha_blending() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let mut r = renderer(&device);
    let structure = [0.8, 0.6, 0.1, 0.4];
    let lead = [0.1, 0.1, 0.9, 1.0];
    let img = render(
        &device,
        &queue,
        &mut r,
        &[Quad::full(0.0, structure)],
        &[Quad {
            z: -0.8,
            x: (-2.0, 0.0),
            colour: lead,
        }],
    );
    assert_rgb_close(pixel(&img, LEFT), over(structure, rgb(lead)), TOL);
    assert_rgb_close(pixel(&img, RIGHT), over(structure, rgb(BACKGROUND)), TOL);
}

#[test]
fn opaque_geometry_in_front_hides_the_atlas_and_geometry_behind_is_tinted() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let mut r = renderer(&device);
    let structure = [0.9, 0.2, 0.2, 0.5];
    let lead = [0.7, 0.7, 0.7, 1.0];
    let img = render(
        &device,
        &queue,
        &mut r,
        &[Quad::full(0.0, structure)],
        &[
            Quad {
                z: 0.5,
                x: (-2.0, 0.0),
                colour: lead,
            },
            Quad {
                z: -0.5,
                x: (0.0, 2.0),
                colour: lead,
            },
        ],
    );
    assert_rgb_close(pixel(&img, LEFT), rgb(lead), TOL);
    assert_rgb_close(pixel(&img, RIGHT), over(structure, rgb(lead)), TOL);
}

#[test]
fn nothing_translucent_leaves_the_opaque_pass_unchanged() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let mut r = renderer(&device);
    let lead = [0.3, 0.6, 0.9, 1.0];
    let img = render(
        &device,
        &queue,
        &mut r,
        &[],
        &[Quad {
            z: 0.0,
            x: (-2.0, 0.0),
            colour: lead,
        }],
    );
    assert_rgb_close(pixel(&img, LEFT), rgb(lead), 1.0 / 255.0);
    assert_rgb_close(pixel(&img, RIGHT), rgb(BACKGROUND), 1.0 / 255.0);
}

#[test]
fn instanced_markers_draw_opaque_and_dimmed_through_the_transparency_pass() {
    use crate::group_scene::{unit_sphere, GroupModels, Instance};

    let Some((device, queue)) = gpu() else {
        return;
    };
    let mut r = renderer(&device);
    let sphere = unit_sphere(12, 18);
    let models = GroupModels::build(&[]);
    let range = models.sphere.expect("sphere mesh");
    let verts: Vec<MeshVert> = models
        .positions
        .iter()
        .zip(&models.normals)
        .map(|(p, n)| MeshVert {
            pos: *p,
            n: *n,
            c: [1.0; 4],
        })
        .collect();
    assert_eq!(verts.len(), sphere.positions.len());
    write_or_grow_buffer(
        &device,
        &queue,
        &mut r.inst_mesh_vb,
        &mut r.inst_mesh_vb_cap,
        bytemuck::cast_slice(&verts),
        wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        "inst_mesh_vb",
    );
    write_or_grow_buffer(
        &device,
        &queue,
        &mut r.inst_mesh_ib,
        &mut r.inst_mesh_ib_cap,
        bytemuck::cast_slice(&models.indices),
        wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        "inst_mesh_ib",
    );
    let at = |x: f32, colour: [f32; 4]| Instance {
        m0: [0.3, 0.0, 0.0, x],
        m1: [0.0, 0.3, 0.0, 0.0],
        m2: [0.0, 0.0, 0.3, 0.0],
        colour,
        extra: [0.0; 4],
    };
    let instances = [
        at(-0.5, [0.9, 0.1, 0.1, 1.0]),
        at(0.5, [0.1, 0.8, 0.2, 0.3]),
    ];
    write_or_grow_buffer(
        &device,
        &queue,
        &mut r.inst_vb,
        &mut r.inst_vb_cap,
        bytemuck::cast_slice(&instances),
        wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        "inst_vb",
    );
    r.inst_draws_opaque = vec![InstDraw {
        range,
        instances: 0..1,
    }];
    r.inst_draws_dimmed = vec![InstDraw {
        range,
        instances: 1..2,
    }];
    let img = render(&device, &queue, &mut r, &[], &[]);
    let left = pixel(&img, LEFT);
    assert!(
        left[0] > 0.5 && left[0] > left[1] + 0.3 && left[0] > left[2] + 0.3,
        "opaque marker should be red: {left:?}"
    );
    let right = pixel(&img, RIGHT);
    let bg = rgb(BACKGROUND);
    assert!(
        right[1] > bg[1] + 0.03 && right[1] < 0.6 && right[2] < bg[2],
        "dimmed marker should tint the background faintly green: {right:?}"
    );
    let corner = pixel(&img, (2, 2));
    assert_rgb_close(corner, bg, 1.0 / 255.0);
}

fn fibre_buffers(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    r: &mut SceneWgpuRenderer,
    segments: &[crate::scene_fibres::FibreSeg],
    width_px: f32,
) {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("fibre_test"),
        size: std::mem::size_of_val(segments) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, bytemuck::cast_slice(segments));
    r.fibre_chunks = vec![(
        buffer,
        segments.len() as u32,
        std::mem::size_of_val(segments) as u64,
    )];
    r.fibre_used = 1;
    r.ribbon_params = [W as f32, H as f32, width_px, 0.0];
}

fn green_fibre() -> Vec<crate::scene_fibres::FibreSeg> {
    vec![
        crate::scene_fibres::FibreSeg {
            p0: [-0.5, 0.0, 0.0],
            p1: [0.0, 0.0, 0.0],
            rgba: [0, 255, 0, 255],
        },
        crate::scene_fibres::FibreSeg {
            p0: [0.0, 0.0, 0.0],
            p1: [0.5, 0.0, 0.0],
            rgba: [0, 255, 0, 255],
        },
    ]
}

#[test]
fn a_fibre_is_a_ribbon_of_the_requested_pixel_width() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let mut r = renderer(&device);
    fibre_buffers(&device, &queue, &mut r, &green_fibre(), 4.0);
    let img = render(&device, &queue, &mut r, &[], &[]);
    let bg = rgb(BACKGROUND);
    for (x, y) in [(16, 31), (16, 32), (48, 31), (48, 32), (32, 31)] {
        assert_rgb_close(pixel(&img, (x, y)), [0.0, 1.0, 0.0], 1.0 / 255.0);
    }
    for (x, y) in [(16, 27), (16, 36), (48, 28), (48, 35)] {
        assert_rgb_close(pixel(&img, (x, y)), bg, 1.0 / 255.0);
    }
    let thick = img
        .iter()
        .enumerate()
        .filter(|(i, p)| i % W as usize == 16 && p[1] > 200 && p[0] < 50)
        .count();
    assert_eq!(thick, 4);
    assert_rgb_close(pixel(&img, (14, 32)), [0.0, 1.0, 0.0], 1.0 / 255.0);
    assert_rgb_close(pixel(&img, (13, 32)), bg, 1.0 / 255.0);
    assert_rgb_close(pixel(&img, (50, 32)), bg, 1.0 / 255.0);
}

#[test]
fn fibres_are_hidden_by_opaque_geometry_in_front_and_tinted_by_translucent_surfaces() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let mut r = renderer(&device);
    fibre_buffers(&device, &queue, &mut r, &green_fibre(), 4.0);
    let blue = [0.1, 0.1, 0.9, 1.0];
    let img = render(
        &device,
        &queue,
        &mut r,
        &[],
        &[Quad {
            z: 0.5,
            x: (0.0, 2.0),
            colour: blue,
        }],
    );
    assert_rgb_close(pixel(&img, LEFT), [0.0, 1.0, 0.0], 1.0 / 255.0);
    assert_rgb_close(pixel(&img, RIGHT), rgb(blue), 1.0 / 255.0);

    let veil = [0.9, 0.1, 0.1, 0.5];
    let img = render(
        &device,
        &queue,
        &mut r,
        &[Quad {
            z: 1.0,
            x: (-2.0, 2.0),
            colour: veil,
        }],
        &[],
    );
    assert_rgb_close(pixel(&img, LEFT), over(veil, [0.0, 1.0, 0.0]), TOL);
    assert_rgb_close(pixel(&img, (16, 8)), over(veil, rgb(BACKGROUND)), TOL);
}

#[test]
fn a_map_overlay_blends_over_its_slice_and_disappears_when_switched_off() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let mut r = renderer(&device);
    let strip = |z: f32| -> Vec<QuadVert> {
        [(-2.0, -2.0), (2.0, -2.0), (-2.0, 2.0), (2.0, 2.0)]
            .iter()
            .map(|&(x, y)| QuadVert {
                pos: [x, y, z],
                uv: [0.5, 0.5],
                _pad: [1.0, 0.0],
            })
            .collect()
    };
    let verts = strip(0.0);
    write_or_grow_buffer(
        &device,
        &queue,
        &mut r.quad_vb,
        &mut r.quad_vb_cap,
        bytemuck::cast_slice(&verts),
        wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        "quad_vb",
    );
    r.quad_ranges = [[0, 4], [0, 0], [0, 0]];
    let texel = [0u8, 0, 255, 128];
    r.overlay_slots[0].resize_if_needed(
        &device,
        &r.quad_layout,
        &r.slice_sampler,
        "map_overlay_axial",
        2,
        2,
    );
    let pixels: Vec<u8> = texel.iter().copied().cycle().take(16).collect();
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &r.overlay_slots[0].tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(2),
        },
        wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
    );
    r.overlay_on = [true, false, false];
    let img = render_plain(&device, &queue, &mut r);
    let want = over([0.0, 0.0, 1.0, 128.0 / 255.0], [0.0; 3]);
    assert_rgb_close(pixel(&img, LEFT), want, TOL);
    assert_rgb_close(pixel(&img, RIGHT), want, TOL);
    r.overlay_on = [false; 3];
    let img = render_plain(&device, &queue, &mut r);
    assert_rgb_close(pixel(&img, LEFT), [0.0; 3], 1.0 / 255.0);
}

fn render_plain(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    r: &mut SceneWgpuRenderer,
) -> Vec<[u8; 4]> {
    r.atlas_indices = 0..0;
    r.electrode_draw_ranges.clear();
    present(device, queue, r)
}

fn settle_overlays(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    r: &mut SceneWgpuRenderer,
    payload: &Scene3DPayload,
) {
    let start = std::time::Instant::now();
    loop {
        r.rebuild_geometry(device, queue, payload);
        let pending = payload
            .overlay_pending
            .load(std::sync::atomic::Ordering::Acquire);
        if pending == 0 && r.overlay_jobs.iter().all(Option::is_none) {
            break;
        }
        assert!(start.elapsed().as_secs() < 20, "overlay did not finish");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn render_payload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    payload: &Scene3DPayload,
    (w, h): (u32, u32),
) -> Vec<u8> {
    let mut r = renderer(device);
    settle_overlays(device, queue, &mut r, payload);
    let sh = payload.shared.lock().unwrap().clone();
    let aspect = w as f32 / h as f32;
    let (vp, eye) = orbit_vp(&sh, aspect);
    r.ribbon_params = [w as f32, h as f32, 3.0, 0.0];
    r.write_globals(queue, vp, eye, view_forward(&sh), false, false, 1.0, 0.5);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    r.encode_scene(device, &mut encoder, [w, h], [false; 3]);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("layers_demo_target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("layers_demo_host"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.08,
                        g: 0.09,
                        b: 0.11,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        r.compositor.composite(&mut pass);
    }
    let bpr = (w * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("layers_demo_readback"),
        size: u64::from(bpr * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    readback.map_async(wgpu::MapMode::Read, .., |res| {
        res.expect("map the readback buffer");
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    let mapped = readback
        .get_mapped_range(..)
        .expect("view the mapped readback buffer");
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h as usize {
        let start = row * bpr as usize;
        out.extend_from_slice(&mapped[start..start + (w * 4) as usize]);
    }
    out
}

fn demo_blob(n: usize) -> Vec<f32> {
    let mut values = vec![0.0_f32; n * n * n];
    let blobs = [
        ([22.0_f32, 30.0, 32.0], 1.0_f32),
        ([42.0, 30.0, 32.0], -0.8),
    ];
    for k in 0..n {
        for j in 0..n {
            for i in 0..n {
                for (c, amp) in blobs {
                    let d2 = (i as f32 - c[0]).powi(2)
                        + (j as f32 - c[1]).powi(2)
                        + (k as f32 - c[2]).powi(2);
                    values[(k * n + j) * n + i] += amp * (-d2 / 40.0).exp();
                }
            }
        }
    }
    values
}

fn demo_fibres(n: usize) -> crate::scene_layers::FibreLayer {
    let mut points = Vec::new();
    let mut offsets = vec![0u32];
    let mut values = Vec::new();
    for f in 0..n {
        let phase = f as f32 / n as f32;
        for p in 0..40 {
            let t = p as f32 / 39.0;
            points.push([
                6.0 + 52.0 * t,
                46.0 + 5.0 * (std::f32::consts::TAU * (phase + t * 0.5)).sin(),
                44.0 + 8.0 * (std::f32::consts::TAU * (phase * 2.0 + t * 0.3)).cos()
                    + 6.0 * (std::f32::consts::PI * t).sin(),
            ]);
        }
        offsets.push(points.len() as u32);
        values.push(1.0 - phase);
    }
    crate::scene_layers::FibreLayer {
        id: "demo".to_owned(),
        label: "demo".to_owned(),
        points,
        offsets,
        values,
        colouring: crate::scene_layers::FibreColouring::Strength,
        total_fibres: n * 5,
    }
}

fn demo_backdrop(n: usize) -> crate::scene_volume::TemplateVolume {
    let mut normed = vec![0.0_f32; n * n * n];
    let c = (n as f32 - 1.0) / 2.0;
    for k in 0..n {
        for j in 0..n {
            for i in 0..n {
                let d = ((i as f32 - c) / 28.0).powi(2)
                    + ((j as f32 - c) / 24.0).powi(2)
                    + ((k as f32 - c) / 20.0).powi(2);
                normed[(k * n + j) * n + i] = if d < 1.0 { 0.35 + 0.4 * (1.0 - d) } else { 0.0 };
            }
        }
    }
    crate::scene_volume::TemplateVolume {
        n0: n,
        n1: n,
        n2: n,
        hdr: nifti::NiftiHeader {
            sform_code: 1,
            srow_x: [1.0, 0.0, 0.0, 0.0],
            srow_y: [0.0, 1.0, 0.0, 0.0],
            srow_z: [0.0, 0.0, 1.0, 0.0],
            ..Default::default()
        },
        normed,
    }
}

#[test]
#[ignore = "renders the layer demo images when DBSLOC_LAYERS_PNG_DIR is set"]
fn layer_demo_images() {
    let Ok(dir) = std::env::var("DBSLOC_LAYERS_PNG_DIR") else {
        return;
    };
    let Some((device, queue)) = gpu() else {
        return;
    };
    let n = 64usize;
    let map = |mode| {
        crate::scene_layers::MapLayer::new(
            "blobs".to_owned(),
            "blobs".to_owned(),
            [n; 3],
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            demo_blob(n),
            mode,
        )
    };
    for (name, mode) in [
        ("isosurface", crate::scene_layers::MapMode::Isosurface),
        ("slices", crate::scene_layers::MapMode::Slices),
    ] {
        let payload = super::atlas_data_tests::layer_payload(&format!("demo_{name}"));
        payload.set_backdrop(std::sync::Arc::new(demo_backdrop(n)));
        {
            let mut sh = payload.shared.lock().unwrap();
            sh.show_axial = true;
            sh.show_coronal = true;
            sh.show_sagittal = true;
            sh.axial_k = 20;
            sh.coronal_j = 30;
            sh.sagittal_i = 32;
            sh.axial_opacity = 0.6;
            sh.coronal_opacity = 0.6;
            sh.sagittal_opacity = 0.6;
            sh.azimuth = 35.0;
            sh.elevation = 25.0;
            sh.target = Vec3::new(32.0, 32.0, 32.0);
            sh.distance = 160.0;
        }
        payload.upsert_fibre_layer(demo_fibres(40));
        payload.upsert_map_layer(map(mode));
        super::atlas_data_tests::wait_for_builds(&payload);
        let pixels = render_payload(&device, &queue, &payload, (640, 480));
        let path = std::path::Path::new(&dir).join(format!("layers_{name}.png"));
        image::save_buffer(&path, &pixels, 640, 480, image::ColorType::Rgba8).unwrap();
        eprintln!("wrote {}", path.display());
    }
}

fn reddish(pixels: &[u8]) -> usize {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| {
            p[0] > 150
                && i32::from(p[0]) - i32::from(p[1]) > 60
                && i32::from(p[0]) - i32::from(p[2]) > 60
        })
        .count()
}

#[test]
fn a_slice_map_overlay_appears_once_its_job_completes() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let n = 64usize;
    let render = |with_map: bool| {
        let payload = super::atlas_data_tests::layer_payload(if with_map {
            "overlay_on"
        } else {
            "overlay_off"
        });
        payload.set_backdrop(std::sync::Arc::new(demo_backdrop(n)));
        {
            let mut sh = payload.shared.lock().unwrap();
            sh.show_axial = true;
            sh.show_coronal = false;
            sh.show_sagittal = false;
            sh.axial_k = 32;
            sh.axial_opacity = 1.0;
            sh.azimuth = 0.0;
            sh.elevation = 80.0;
            sh.target = Vec3::new(32.0, 32.0, 32.0);
            sh.distance = 160.0;
        }
        if with_map {
            payload.upsert_map_layer(crate::scene_layers::MapLayer::new(
                "blobs".to_owned(),
                "blobs".to_owned(),
                [n; 3],
                [
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                    [0.0, 0.0, 0.0, 1.0],
                ],
                demo_blob(n),
                crate::scene_layers::MapMode::Slices,
            ));
            super::atlas_data_tests::wait_for_builds(&payload);
        }
        render_payload(&device, &queue, &payload, (320, 240))
    };
    let with = reddish(&render(true));
    let without = reddish(&render(false));
    assert!(with > without + 50, "overlay pixels {with} vs {without}");
}
