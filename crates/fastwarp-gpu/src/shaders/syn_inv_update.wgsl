// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

struct P {
    nx: u32,
    ny: u32,
    nz: u32,
    n: u32,
    sp: vec4<f32>,
    eps: f32,
    enforce: u32,
    pad0: u32,
    pad1: u32,
}

struct St {
    max_err: f32,
    mean_err: f32,
    executed: u32,
    run: u32,
}

@group(0) @binding(0) var<storage, read_write> psi: array<f32>;
@group(0) @binding(1) var<storage, read> err: array<f32>;
@group(0) @binding(2) var<storage, read> st: St;
@group(0) @binding(3) var<uniform> p: P;

@compute @workgroup_size(8, 8, 4)
fn k_inv_update(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.nx || id.y >= p.ny || id.z >= p.nz) {
        return;
    }
    if (st.run == 0u) {
        return;
    }
    let i = id.x + p.nx * (id.y + p.ny * id.z);
    let e = vec3<f32>(err[i], err[p.n + i], err[2u * p.n + i]);
    let limit = p.eps * st.max_err;
    let nrm = length(e / p.sp.xyz);
    var upd = -e;
    if (nrm > limit) {
        upd = upd * (limit / nrm);
    }
    var v = vec3<f32>(psi[i], psi[p.n + i], psi[2u * p.n + i]) + upd * p.eps;
    if (p.enforce != 0u) {
        if (id.x == 0u || id.y == 0u || id.z == 0u || id.x == p.nx - 1u || id.y == p.ny - 1u || id.z == p.nz - 1u) {
            v = vec3<f32>(0.0);
        }
    }
    psi[i] = v.x;
    psi[p.n + i] = v.y;
    psi[2u * p.n + i] = v.z;
}
