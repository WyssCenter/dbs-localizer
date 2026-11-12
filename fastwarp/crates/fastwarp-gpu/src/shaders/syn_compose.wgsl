// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

struct P {
    nx: u32,
    ny: u32,
    nz: u32,
    n: u32,
    l0: vec4<f32>,
    l1: vec4<f32>,
    l2: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> src: array<f32>;
@group(0) @binding(1) var<storage, read> warp: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;
@group(0) @binding(3) var<uniform> p: P;

@compute @workgroup_size(8, 8, 4)
fn k_compose(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.nx || id.y >= p.ny || id.z >= p.nz) {
        return;
    }
    let i = id.x + p.nx * (id.y + p.ny * id.z);
    let w = vec3<f32>(warp[i], warp[p.n + i], warp[2u * p.n + i]);
    let c = vec3<f32>(id) + vec3<f32>(dot(p.l0.xyz, w), dot(p.l1.xyz, w), dot(p.l2.xyz, w));
    var d = vec3<f32>(0.0);
    if (inside(c, p.nx, p.ny, p.nz)) {
        d = vec3<f32>(
            tri(c, 0u, p.nx, p.ny, p.nz),
            tri(c, p.n, p.nx, p.ny, p.nz),
            tri(c, 2u * p.n, p.nx, p.ny, p.nz),
        );
    }
    dst[i] = w.x + d.x;
    dst[p.n + i] = w.y + d.y;
    dst[2u * p.n + i] = w.z + d.z;
}
