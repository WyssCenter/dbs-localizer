// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

struct Params {
    n: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<storage, read> row_ptr: array<u32>;
@group(0) @binding(1) var<storage, read> cols: array<u32>;
@group(0) @binding(2) var<storage, read> vals: array<f32>;
@group(0) @binding(3) var<storage, read> diag: array<f32>;
@group(0) @binding(4) var<storage, read> x: array<f32>;
@group(0) @binding(5) var<storage, read_write> y: array<f32>;
@group(0) @binding(6) var<storage, read_write> partials: array<f32>;
@group(0) @binding(7) var<uniform> p: Params;

var<workgroup> sh: array<f32, 256>;

@compute @workgroup_size(256)
fn k_vat_spmv(
    @builtin(global_invocation_id) g: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) wg: vec3<u32>,
) {
    let r = g.x;
    var dot_term = 0.0;
    if (r < p.n) {
        var s = diag[r] * x[r];
        let end = row_ptr[r + 1u];
        for (var e = row_ptr[r]; e < end; e = e + 1u) {
            s = s + vals[e] * x[cols[e]];
        }
        y[r] = s;
        dot_term = x[r] * s;
    }
    sh[li] = dot_term;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (li < s) {
            sh[li] = sh[li] + sh[li + s];
        }
        workgroupBarrier();
    }
    if (li == 0u) {
        partials[wg.x] = sh[0];
    }
}
