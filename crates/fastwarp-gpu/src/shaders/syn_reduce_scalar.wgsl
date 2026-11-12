// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

struct P {
    count: u32,
    mode: u32,
    out_index: u32,
    pad0: u32,
}

@group(0) @binding(0) var<storage, read> part: array<f32>;
@group(0) @binding(1) var<storage, read_write> scalar: array<f32>;
@group(0) @binding(2) var<uniform> p: P;

var<workgroup> sh: array<f32, 256>;

@compute @workgroup_size(256)
fn k_reduce_scalar(@builtin(local_invocation_index) li: u32) {
    var acc = 0.0;
    for (var k = li; k < p.count; k = k + 256u) {
        if (p.mode == 0u) {
            acc = max(acc, part[k]);
        } else {
            acc = acc + part[k];
        }
    }
    sh[li] = acc;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (li < s) {
            if (p.mode == 0u) {
                sh[li] = max(sh[li], sh[li + s]);
            } else {
                sh[li] = sh[li] + sh[li + s];
            }
        }
        workgroupBarrier();
    }
    if (li == 0u) {
        scalar[p.out_index] = sh[0];
    }
}
