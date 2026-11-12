// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

struct P {
    nx: u32,
    ny: u32,
    nz: u32,
    n: u32,
    snx: u32,
    sny: u32,
    snz: u32,
    sn: u32,
    flags: u32,
    nplanes: u32,
    pad0: u32,
    pad1: u32,
    mi0: vec4<f32>,
    mi1: vec4<f32>,
    mi2: vec4<f32>,
    l0: vec4<f32>,
    l1: vec4<f32>,
    l2: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> src: array<f32>;
@group(0) @binding(1) var<storage, read> field: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;
@group(0) @binding(3) var<uniform> p: P;

@compute @workgroup_size(8, 8, 4)
fn k_warp(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.nx || id.y >= p.ny || id.z >= p.nz) {
        return;
    }
    let i = id.x + p.nx * (id.y + p.ny * id.z);
    let idx = vec3<f32>(id);
    var c = vec3<f32>(
        dot(p.mi0.xyz, idx) + p.mi0.w,
        dot(p.mi1.xyz, idx) + p.mi1.w,
        dot(p.mi2.xyz, idx) + p.mi2.w,
    );
    if ((p.flags & 1u) != 0u) {
        let u = vec3<f32>(field[i], field[p.n + i], field[2u * p.n + i]);
        c = c + vec3<f32>(dot(p.l0.xyz, u), dot(p.l1.xyz, u), dot(p.l2.xyz, u));
    }
    let ok = inside(c, p.snx, p.sny, p.snz);
    for (var k = 0u; k < p.nplanes; k = k + 1u) {
        var v = 0.0;
        if (ok) {
            v = tri(c, k * p.sn, p.snx, p.sny, p.snz);
        }
        dst[k * p.n + i] = v;
    }
}
