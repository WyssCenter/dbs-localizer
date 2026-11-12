// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

struct Params {
    n: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<storage, read> r: array<f32>;
@group(0) @binding(1) var<storage, read> inv_diag: array<f32>;
@group(0) @binding(2) var<storage, read_write> pdir: array<f32>;
@group(0) @binding(3) var<storage, read> sc: array<f32>;
@group(0) @binding(4) var<uniform> p: Params;

@compute @workgroup_size(256)
fn k_vat_pupdate(@builtin(global_invocation_id) g: vec3<u32>) {
    let i = g.x;
    if (i < p.n) {
        pdir[i] = r[i] * inv_diag[i] + sc[2] * pdir[i];
    }
}
