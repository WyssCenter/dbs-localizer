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
    sp: vec4<f32>,
}

struct St {
    max_err: f32,
    mean_err: f32,
    executed: u32,
    run: u32,
}

@group(0) @binding(0) var<storage, read> src: array<f32>;
@group(0) @binding(1) var<storage, read> warp: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;
@group(0) @binding(3) var<storage, read_write> part: array<f32>;
@group(0) @binding(4) var<storage, read> st: St;
@group(0) @binding(5) var<uniform> p: P;

var<workgroup> sh_sum: array<f32, 256>;
var<workgroup> sh_max: array<f32, 256>;

@compute @workgroup_size(8, 8, 4)
fn k_inv_residual(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) w: vec3<u32>,
    @builtin(num_workgroups) nw: vec3<u32>,
) {
    let act = st.run != 0u;
    var nrm = 0.0;
    if (act && id.x < p.nx && id.y < p.ny && id.z < p.nz) {
        let i = id.x + p.nx * (id.y + p.ny * id.z);
        let wv = vec3<f32>(warp[i], warp[p.n + i], warp[2u * p.n + i]);
        let c = vec3<f32>(id) + vec3<f32>(dot(p.l0.xyz, wv), dot(p.l1.xyz, wv), dot(p.l2.xyz, wv));
        var d = vec3<f32>(0.0);
        if (inside(c, p.nx, p.ny, p.nz)) {
            d = vec3<f32>(
                tri(c, 0u, p.nx, p.ny, p.nz),
                tri(c, p.n, p.nx, p.ny, p.nz),
                tri(c, 2u * p.n, p.nx, p.ny, p.nz),
            );
        }
        let e = wv + d;
        dst[i] = e.x;
        dst[p.n + i] = e.y;
        dst[2u * p.n + i] = e.z;
        nrm = length(e / p.sp.xyz);
    }
    sh_sum[li] = nrm;
    sh_max[li] = nrm;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (li < s) {
            sh_sum[li] = sh_sum[li] + sh_sum[li + s];
            sh_max[li] = max(sh_max[li], sh_max[li + s]);
        }
        workgroupBarrier();
    }
    if (li == 0u && act) {
        let count = nw.x * nw.y * nw.z;
        let wi = w.x + nw.x * (w.y + nw.y * w.z);
        part[wi] = sh_sum[0];
        part[count + wi] = sh_max[0];
    }
}
