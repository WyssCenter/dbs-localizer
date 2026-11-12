// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

struct P {
    n: u32,
    mode: u32,
    pad0: u32,
    pad1: u32,
    sp: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> field: array<f32>;
@group(0) @binding(1) var<storage, read_write> part: array<f32>;
@group(0) @binding(2) var<uniform> p: P;

var<workgroup> sh: array<f32, 256>;

@compute @workgroup_size(256)
fn k_norm_partial(
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) w: vec3<u32>,
    @builtin(num_workgroups) nw: vec3<u32>,
) {
    let g = (w.y * nw.x + w.x) * 256u + li;
    var v = 0.0;
    if (g < p.n) {
        let u = vec3<f32>(field[g], field[p.n + g], field[2u * p.n + g]);
        if (p.mode == 0u) {
            v = length(u / p.sp.xyz);
        } else {
            v = dot(u, u);
        }
    }
    sh[li] = v;
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
        part[w.y * nw.x + w.x] = sh[0];
    }
}
