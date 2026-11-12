// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

struct P {
    total: u32,
    a: f32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<storage, read_write> y: array<f32>;
@group(0) @binding(1) var<uniform> p: P;

@compute @workgroup_size(256)
fn k_scale(
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) w: vec3<u32>,
    @builtin(num_workgroups) nw: vec3<u32>,
) {
    let i = (w.y * nw.x + w.x) * 256u + li;
    if (i < p.total) {
        y[i] = y[i] * p.a;
    }
}
