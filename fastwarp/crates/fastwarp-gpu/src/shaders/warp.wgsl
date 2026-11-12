// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

struct Warp {
    nx: u32,
    ny: u32,
    nz: u32,
    n: u32,
    snx: u32,
    sny: u32,
    snz: u32,
    flags: u32,
    vo: vec4<f32>,
    vs: vec4<f32>,
    so: vec4<f32>,
    ss: vec4<f32>,
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> src: array<f32>;
@group(0) @binding(1) var<storage, read> ux: array<f32>;
@group(0) @binding(2) var<storage, read> uy: array<f32>;
@group(0) @binding(3) var<storage, read> uz: array<f32>;
@group(0) @binding(4) var<storage, read_write> dst: array<f32>;
@group(0) @binding(5) var<storage, read_write> valid: array<u32>;
@group(0) @binding(6) var<uniform> w: Warp;

@compute @workgroup_size(8, 8, 4)
fn k_warp_scalar(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= w.nx || id.y >= w.ny || id.z >= w.nz) {
        return;
    }
    let i = id.x + w.nx * (id.y + w.ny * id.z);
    var p = w.vo.xyz + w.vs.xyz * vec3<f32>(id);
    if ((w.flags & 1u) != 0u) {
        p = p + vec3<f32>(ux[i], uy[i], uz[i]);
    }
    let q = vec3<f32>(
        dot(w.m0.xyz, p) + w.m0.w,
        dot(w.m1.xyz, p) + w.m1.w,
        dot(w.m2.xyz, p) + w.m2.w,
    );
    let c = (q - w.so.xyz) / w.ss.xyz;
    let hi = vec3<f32>(f32(w.snx - 1u), f32(w.sny - 1u), f32(w.snz - 1u));
    if (any(c < vec3<f32>(0.0)) || any(c > hi)) {
        dst[i] = 0.0;
        valid[i] = 0u;
        return;
    }
    let f = floor(c);
    let t = c - f;
    let x0 = u32(f.x);
    let y0 = u32(f.y);
    let z0 = u32(f.z);
    let x1 = min(x0 + 1u, w.snx - 1u);
    let y1 = min(y0 + 1u, w.sny - 1u);
    let z1 = min(z0 + 1u, w.snz - 1u);
    let sx = w.snx;
    let sxy = w.snx * w.sny;
    let b00 = y0 * sx + z0 * sxy;
    let b10 = y1 * sx + z0 * sxy;
    let b01 = y0 * sx + z1 * sxy;
    let b11 = y1 * sx + z1 * sxy;
    let c00 = mix(src[b00 + x0], src[b00 + x1], t.x);
    let c10 = mix(src[b10 + x0], src[b10 + x1], t.x);
    let c01 = mix(src[b01 + x0], src[b01 + x1], t.x);
    let c11 = mix(src[b11 + x0], src[b11 + x1], t.x);
    let c0 = mix(c00, c10, t.y);
    let c1 = mix(c01, c11, t.y);
    dst[i] = mix(c0, c1, t.z);
    valid[i] = 1u;
}
