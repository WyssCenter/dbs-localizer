// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

struct Globals {
    view_proj: mat4x4<f32>,
    light_dir: vec4<f32>,
    cam_pos: vec4<f32>,
    atlas_unlit: f32,
    atlas_flat_normals: f32,
    slice_window: f32,
    slice_level: f32,
    oit_frame: vec4<f32>,
    view_fwd: vec4<f32>,
    ribbon: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> g: Globals;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) n: vec3<f32>,
    @location(2) color: vec4<f32>,
}

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) wpos: vec3<f32>,
    @location(1) n: vec3<f32>,
    @location(2) color: vec4<f32>,
}

@vertex
fn vs_main(v: VIn) -> VOut {
    var o: VOut;
    let wp = v.pos;
    o.wpos = wp;
    o.n = v.n;
    o.color = v.color;
    o.clip = g.view_proj * vec4<f32>(wp, 1.0);
    return o;
}

fn shade(i: VOut) -> vec4<f32> {
    if (g.atlas_unlit > 0.5) {
        return vec4<f32>(i.color.rgb, i.color.a);
    }
    let ld = normalize(g.light_dir.xyz);
    var nn = normalize(i.n);
    if (g.atlas_flat_normals > 0.5) {
        nn = normalize(cross(dpdx(i.wpos), dpdy(i.wpos)));
    }
    let ndl = abs(dot(nn, ld));
    let amb = 0.40;
    let diff = 0.60 * ndl;
    let light = amb + diff;
    let rgb = i.color.rgb * light;
    return vec4<f32>(rgb, i.color.a);
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    return shade(i);
}

fn oit_weight(wpos: vec3<f32>, alpha: f32) -> f32 {
    let d = dot(wpos - g.oit_frame.xyz, g.view_fwd.xyz);
    let u = clamp(0.5 + 0.5 * d / max(g.oit_frame.w, 1e-3), 0.0, 1.0);
    let near = 1.0 - u;
    return alpha * clamp(8.0 * near * near * near, 1e-2, 8.0);
}

struct OitOut {
    @location(0) accum: vec4<f32>,
    @location(1) reveal: f32,
}

@fragment
fn fs_atlas_oit(i: VOut) -> OitOut {
    let c = shade(i);
    if (c.a <= 0.0) {
        discard;
    }
    let w = oit_weight(i.wpos, c.a);
    var o: OitOut;
    o.accum = vec4<f32>(c.rgb * c.a, c.a) * w;
    o.reveal = c.a;
    return o;
}

@group(1) @binding(0)
var slice_tex: texture_2d<f32>;
@group(1) @binding(1)
var slice_samp: sampler;

struct QIn {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) alpha: f32,
}

struct QOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
}

@vertex
fn vs_quad(v: QIn) -> QOut {
    var o: QOut;
    o.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
    o.uv = v.uv;
    o.alpha = v.alpha;
    return o;
}

@fragment
fn fs_quad(i: QOut) -> @location(0) vec4<f32> {
    let raw = textureSample(slice_tex, slice_samp, i.uv).r;
    let lo = g.slice_level - 0.5 * g.slice_window;
    let gv = clamp((raw - lo) / max(g.slice_window, 1e-3), 0.0, 1.0);
    return vec4<f32>(gv, gv, gv, i.alpha);
}

@fragment
fn fs_quad_rgba(i: QOut) -> @location(0) vec4<f32> {
    return textureSample(slice_tex, slice_samp, i.uv);
}

struct FibreIn {
    @location(0) p0: vec3<f32>,
    @location(1) p1: vec3<f32>,
    @location(2) color: vec4<f32>,
}

struct FibreOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_fibre(@builtin(vertex_index) vi: u32, s: FibreIn) -> FibreOut {
    var o: FibreOut;
    o.color = s.color;
    var c0 = g.view_proj * vec4<f32>(s.p0, 1.0);
    var c1 = g.view_proj * vec4<f32>(s.p1, 1.0);
    let in0 = c0.z >= 0.0 && c0.w > 1e-4;
    let in1 = c1.z >= 0.0 && c1.w > 1e-4;
    if (!in0 && !in1) {
        o.clip = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        return o;
    }
    if (!in0) {
        let t = c0.z / (c0.z - c1.z);
        c0 = mix(c0, c1, clamp(t, 0.0, 1.0));
    } else if (!in1) {
        let t = c1.z / (c1.z - c0.z);
        c1 = mix(c1, c0, clamp(t, 0.0, 1.0));
    }
    if (c0.w <= 1e-4 || c1.w <= 1e-4) {
        o.clip = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        return o;
    }
    let half_px = 0.5 * g.ribbon.xy;
    let a = c0.xy / c0.w * half_px;
    let b = c1.xy / c1.w * half_px;
    var d = b - a;
    let len = length(d);
    if (len > 1e-4) {
        d = d / len;
    } else {
        d = vec2<f32>(1.0, 0.0);
    }
    let nrm = vec2<f32>(-d.y, d.x);
    let hw = 0.5 * g.ribbon.z;
    var ends = array<f32, 6>(0.0, 0.0, 1.0, 1.0, 0.0, 1.0);
    var sides = array<f32, 6>(-1.0, 1.0, -1.0, -1.0, 1.0, 1.0);
    let end = ends[vi];
    let side = sides[vi];
    var clip = mix(c0, c1, end);
    let offset_px = nrm * (side * hw) + d * ((2.0 * end - 1.0) * hw);
    clip = vec4<f32>(clip.xy + offset_px / half_px * clip.w, clip.z, clip.w);
    o.clip = clip;
    return o;
}

@fragment
fn fs_fibre(i: FibreOut) -> @location(0) vec4<f32> {
    return vec4<f32>(i.color.rgb, 1.0);
}

struct LIn {
    @location(0) pos: vec3<f32>,
}

@vertex
fn vs_line(v: LIn) -> @builtin(position) vec4<f32> {
    return g.view_proj * vec4<f32>(v.pos, 1.0);
}

@fragment
fn fs_line() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 0.35, 0.12, 1.0);
}

struct InstIn {
    @location(3) m0: vec4<f32>,
    @location(4) m1: vec4<f32>,
    @location(5) m2: vec4<f32>,
    @location(6) icolor: vec4<f32>,
    @location(7) extra: vec4<f32>,
}

struct IOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) wpos: vec3<f32>,
    @location(1) n: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) rim: f32,
}

@vertex
fn vs_inst(v: VIn, i: InstIn) -> IOut {
    var o: IOut;
    let p = vec4<f32>(v.pos, 1.0);
    let wp = vec3<f32>(dot(i.m0, p), dot(i.m1, p), dot(i.m2, p));
    o.wpos = wp;
    o.n = vec3<f32>(dot(i.m0.xyz, v.n), dot(i.m1.xyz, v.n), dot(i.m2.xyz, v.n));
    o.color = v.color * i.icolor;
    o.rim = i.extra.x;
    o.clip = g.view_proj * vec4<f32>(wp, 1.0);
    return o;
}

fn shade_inst(i: IOut) -> vec4<f32> {
    var base: VOut;
    base.clip = i.clip;
    base.wpos = i.wpos;
    base.n = i.n;
    base.color = i.color;
    var c = shade(base);
    if (i.rim > 0.0) {
        let view = normalize(g.cam_pos.xyz - i.wpos);
        let edge = pow(1.0 - abs(dot(normalize(i.n), view)), 2.5);
        c = vec4<f32>(mix(c.rgb, vec3<f32>(0.9, 0.9, 0.7), clamp(edge * i.rim * 1.6, 0.0, 1.0)), c.a);
    }
    return c;
}

@fragment
fn fs_inst(i: IOut) -> @location(0) vec4<f32> {
    return shade_inst(i);
}

@fragment
fn fs_inst_oit(i: IOut) -> OitOut {
    let c = shade_inst(i);
    if (c.a <= 0.0) {
        discard;
    }
    let w = oit_weight(i.wpos, c.a);
    var o: OitOut;
    o.accum = vec4<f32>(c.rgb * c.a, c.a) * w;
    o.reveal = c.a;
    return o;
}
