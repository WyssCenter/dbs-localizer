// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

struct P {
    max_tol: f32,
    mean_tol: f32,
    pad0: u32,
    pad1: u32,
}

struct St {
    max_err: f32,
    mean_err: f32,
    executed: u32,
    run: u32,
}

@group(0) @binding(0) var<storage, read_write> st: St;
@group(0) @binding(1) var<uniform> p: P;

@compute @workgroup_size(1)
fn k_inv_begin() {
    if (st.run != 0u && !(st.max_err > p.max_tol && st.mean_err > p.mean_tol)) {
        st.run = 0u;
    }
}
