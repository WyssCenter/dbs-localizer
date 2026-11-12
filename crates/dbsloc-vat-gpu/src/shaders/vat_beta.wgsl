// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

struct Params {
    count: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<storage, read> partials: array<f32>;
@group(0) @binding(1) var<storage, read_write> sc: array<f32>;
@group(0) @binding(2) var<uniform> p: Params;

var<workgroup> sh_rr: array<f32, 256>;
var<workgroup> sh_rz: array<f32, 256>;

@compute @workgroup_size(256)
fn k_vat_beta(@builtin(local_invocation_index) li: u32) {
    var rr = 0.0;
    var rz = 0.0;
    for (var k = li; k < p.count; k = k + 256u) {
        rr = rr + partials[k];
        rz = rz + partials[p.count + k];
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
        let rz_old = sc[0];
        let rz_new = sh_rz[0];
        var beta = 0.0;
        if (rz_old > 0.0) {
            beta = rz_new / rz_old;
        }
        sc[0] = rz_new;
        sc[2] = beta;
        sc[4] = sh_rr[0];
    }
}
