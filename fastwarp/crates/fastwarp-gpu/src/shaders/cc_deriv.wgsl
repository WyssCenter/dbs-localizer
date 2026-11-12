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

const EPS: f32 = 1e-9;

@group(0) @binding(0) var<storage, read> sums: array<f32>;
@group(0) @binding(1) var<storage, read> inter: array<f32>;
@group(0) @binding(2) var<storage, read_write> ux: array<f32>;
@group(0) @binding(3) var<storage, read_write> uy: array<f32>;
@group(0) @binding(4) var<storage, read_write> uz: array<f32>;
@group(0) @binding(5) var<storage, read_write> partials: array<f32>;
@group(0) @binding(6) var<uniform> g: Grid;

var<workgroup> sh: array<f32, 256>;

@compute @workgroup_size(8, 8, 4)
fn k_cc_value_derivative(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) w: vec3<u32>,
    @builtin(num_workgroups) nw: vec3<u32>,
) {
    var val = 0.0;
    if (id.x < g.nx && id.y < g.ny && id.z < g.nz) {
        let i = id.x + g.nx * (id.y + g.ny * id.z);
        var dx = 0.0;
        var dy = 0.0;
        var dz = 0.0;
        if (inter[5u * g.n + i] > 0.5) {
            let cnt = sums[5u * g.n + i];
            let si = sums[i];
            let sj = sums[g.n + i];
            let sff = sums[2u * g.n + i] - si * si / cnt;
            let smm = sums[3u * g.n + i] - sj * sj / cnt;
            let sfm = sums[4u * g.n + i] - si * sj / cnt;
            let den = sff * smm;
            if (den > EPS) {
                val = -(sfm * sfm / den);
                let ac = inter[i] - si / cnt;
                let bc = inter[g.n + i] - sj / cnt;
                let c = 2.0 * sfm / den * (ac - sfm / smm * bc);
                var gx = 0.0;
                var gy = 0.0;
                var gz = 0.0;
                if (id.x > 0u && id.x + 1u < g.nx) {
                    gx = 0.5 * (inter[g.n + i + 1u] - inter[g.n + i - 1u]);
                }
                if (id.y > 0u && id.y + 1u < g.ny) {
                    gy = 0.5 * (inter[g.n + i + g.nx] - inter[g.n + i - g.nx]);
                }
                if (id.z > 0u && id.z + 1u < g.nz) {
                    let sz = g.nx * g.ny;
                    gz = 0.5 * (inter[g.n + i + sz] - inter[g.n + i - sz]);
                }
                dx = c * gx;
                dy = c * gy;
                dz = c * gz;
            } else {
                val = -1.0;
            }
        }
        ux[i] = dx;
        uy[i] = dy;
        uz[i] = dz;
    }
    sh[li] = val;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (li < s) {
            sh[li] = sh[li] + sh[li + s];
        }
        workgroupBarrier();
    }
    if (li == 0u) {
        partials[w.x + nw.x * (w.y + nw.y * w.z)] = sh[0];
    }
}
