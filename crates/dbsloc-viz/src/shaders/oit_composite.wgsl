// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

@group(0) @binding(0)
var opaque_tex: texture_2d<f32>;
@group(0) @binding(1)
var accum_tex: texture_2d<f32>;
@group(0) @binding(2)
var reveal_tex: texture_2d<f32>;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> VOut {
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    var o: VOut;
    o.clip = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    o.uv = vec2<f32>(x, y);
    return o;
}

fn texel(t: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    let dims = vec2<i32>(textureDimensions(t));
    let p = clamp(vec2<i32>(floor(uv * vec2<f32>(dims))), vec2<i32>(0), dims - vec2<i32>(1));
    return textureLoad(t, p, 0);
}

@fragment
fn fs_composite(i: VOut) -> @location(0) vec4<f32> {
    let base = texel(opaque_tex, i.uv);
    let acc = texel(accum_tex, i.uv);
    let reveal = clamp(texel(reveal_tex, i.uv).r, 0.0, 1.0);
    let cover = 1.0 - reveal;
    let avg = acc.rgb / max(acc.a, 1e-5);
    return vec4<f32>(avg * cover + base.rgb * reveal, cover + base.a * reveal);
}
