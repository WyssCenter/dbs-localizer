// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

struct Params {
    n: u32,
    a: f32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<storage, read> x: array<f32>;
@group(0) @binding(1) var<storage, read_write> y: array<f32>;
@group(0) @binding(2) var<uniform> p: Params;

@compute @workgroup_size(256)
fn k_axpy(@builtin(global_invocation_id) g: vec3<u32>) {
    let i = g.x;
    if (i < p.n) {
        y[i] = p.a * x[i] + y[i];
    }
}
