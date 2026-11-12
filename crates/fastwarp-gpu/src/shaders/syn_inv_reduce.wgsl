// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

struct P {
    count: u32,
    n: u32,
    pad0: u32,
    pad1: u32,
}

struct St {
    max_err: f32,
    mean_err: f32,
    executed: u32,
    run: u32,
}

@group(0) @binding(0) var<storage, read> part: array<f32>;
@group(0) @binding(1) var<storage, read_write> st: St;
@group(0) @binding(2) var<uniform> p: P;

var<workgroup> sh_sum: array<f32, 256>;
var<workgroup> sh_max: array<f32, 256>;

@compute @workgroup_size(256)
fn k_inv_reduce(@builtin(local_invocation_index) li: u32) {
    let act = st.run != 0u;
    var s = 0.0;
    var m = 0.0;
    for (var k = li; k < p.count; k = k + 256u) {
        s = s + part[k];
        m = max(m, part[p.count + k]);
    }
    sh_sum[li] = s;
    sh_max[li] = m;
    workgroupBarrier();
    for (var h = 128u; h > 0u; h = h >> 1u) {
        if (li < h) {
            sh_sum[li] = sh_sum[li] + sh_sum[li + h];
            sh_max[li] = max(sh_max[li], sh_max[li + h]);
        }
        workgroupBarrier();
    }
    if (li == 0u && act) {
        st.mean_err = sh_sum[0] / f32(p.n);
        st.max_err = sh_max[0];
        st.executed = st.executed + 1u;
    }
}
