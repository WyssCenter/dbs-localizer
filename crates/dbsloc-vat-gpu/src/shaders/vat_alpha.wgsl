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

var<workgroup> sh: array<f32, 256>;

@compute @workgroup_size(256)
fn k_vat_alpha(@builtin(local_invocation_index) li: u32) {
    var acc = 0.0;
    for (var k = li; k < p.count; k = k + 256u) {
        acc = acc + partials[k];
    }
    sh[li] = acc;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (li < s) {
            sh[li] = sh[li] + sh[li + s];
        }
        workgroupBarrier();
    }
    if (li == 0u) {
        let pap = sh[0];
        let rz = sc[0];
        var alpha = 0.0;
        if (pap > 0.0 && rz > 0.0) {
            alpha = rz / pap;
        }
        sc[1] = alpha;
        sc[3] = pap;
    }
}
