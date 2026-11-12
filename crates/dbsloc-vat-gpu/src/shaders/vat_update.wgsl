// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

struct Params {
    n: u32,
    groups: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<storage, read> pdir: array<f32>;
@group(0) @binding(1) var<storage, read> ap: array<f32>;
@group(0) @binding(2) var<storage, read_write> x: array<f32>;
@group(0) @binding(3) var<storage, read_write> r: array<f32>;
@group(0) @binding(4) var<storage, read> inv_diag: array<f32>;
@group(0) @binding(5) var<storage, read> sc: array<f32>;
@group(0) @binding(6) var<storage, read_write> partials: array<f32>;
@group(0) @binding(7) var<uniform> p: Params;

var<workgroup> sh_rr: array<f32, 256>;
var<workgroup> sh_rz: array<f32, 256>;

@compute @workgroup_size(256)
fn k_vat_update(
    @builtin(global_invocation_id) g: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) wg: vec3<u32>,
) {
    let i = g.x;
    var rr = 0.0;
    var rz = 0.0;
    if (i < p.n) {
        let alpha = sc[1];
        x[i] = x[i] + alpha * pdir[i];
        let ri = r[i] - alpha * ap[i];
        r[i] = ri;
        rr = ri * ri;
        rz = ri * ri * inv_diag[i];
    }
    sh_rr[li] = rr;
    sh_rz[li] = rz;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (li < s) {
            sh_rr[li] = sh_rr[li] + sh_rr[li + s];
            sh_rz[li] = sh_rz[li] + sh_rz[li + s];
        }
        workgroupBarrier();
    }
    if (li == 0u) {
        partials[wg.x] = sh_rr[0];
        partials[p.groups + wg.x] = sh_rz[0];
    }
}
