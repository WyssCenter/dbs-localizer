// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

struct Grid {
    nx: u32,
    ny: u32,
    nz: u32,
    n: u32,
    shift_i: f32,
    shift_j: f32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<storage, read> img_i: array<f32>;
@group(0) @binding(1) var<storage, read> img_j: array<f32>;
@group(0) @binding(2) var<storage, read> mask: array<u32>;
@group(0) @binding(3) var<storage, read_write> planes: array<f32>;
@group(0) @binding(4) var<uniform> g: Grid;

@compute @workgroup_size(8, 8, 4)
fn k_cc_intermediates(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= g.nx || id.y >= g.ny || id.z >= g.nz) {
        return;
    }
    let i = id.x + g.nx * (id.y + g.ny * id.z);
    let m = f32(mask[i]);
    let a = (img_i[i] - g.shift_i) * m;
    let b = (img_j[i] - g.shift_j) * m;
    planes[i] = a;
    planes[g.n + i] = b;
    planes[2u * g.n + i] = a * a;
    planes[3u * g.n + i] = b * b;
    planes[4u * g.n + i] = a * b;
    planes[5u * g.n + i] = m;
}
