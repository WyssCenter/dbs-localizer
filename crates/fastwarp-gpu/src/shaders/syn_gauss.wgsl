// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

struct P {
    nx: u32,
    ny: u32,
    nz: u32,
    n: u32,
    axis: u32,
    len: u32,
    stride: u32,
    radius: u32,
    tap_off: u32,
    fin: u32,
    pad0: u32,
    pad1: u32,
    w1: f32,
    w2: f32,
    pad2: f32,
    pad3: f32,
}

@group(0) @binding(0) var<storage, read> src: array<f32>;
@group(0) @binding(1) var<storage, read> taps: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;
@group(0) @binding(3) var<storage, read> orig: array<f32>;
@group(0) @binding(4) var<uniform> p: P;

@compute @workgroup_size(32, 4, 2)
fn k_gauss_axis(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.nx || id.y >= p.ny || id.z >= p.nz) {
        return;
    }
    let i = id.x + p.nx * (id.y + p.ny * id.z);
    var pos = id.x;
    if (p.axis == 1u) {
        pos = id.y;
    } else if (p.axis == 2u) {
        pos = id.z;
    }
    let base = i - pos * p.stride;
    let last = i32(p.len) - 1;
    let taps_n = 2u * p.radius + 1u;
    var boundary = false;
    if (p.fin != 0u) {
        boundary = id.x == 0u || id.y == 0u || id.z == 0u || id.x == p.nx - 1u || id.y == p.ny - 1u || id.z == p.nz - 1u;
    }
    for (var k = 0u; k < 3u; k = k + 1u) {
        var res = 0.0;
        if (!boundary) {
            var acc = 0.0;
            for (var t = 0u; t < taps_n; t = t + 1u) {
                let q = clamp(i32(pos) + i32(t) - i32(p.radius), 0, last);
                acc = acc + taps[p.tap_off + t] * src[k * p.n + base + u32(q) * p.stride];
            }
            res = acc;
            if (p.fin != 0u) {
                res = acc * p.w1 + orig[k * p.n + i] * p.w2;
            }
        }
        dst[k * p.n + i] = res;
    }
}

const TILE_X: u32 = 256u;
const TILE_X_WIDTH: u32 = 272u;
const SEG: u32 = 32u;

var<workgroup> tile_x: array<f32, 272>;
var<workgroup> tile_yz: array<f32, 1536>;

fn gauss_finish(acc: f32, orig_index: u32) -> f32 {
    if (p.fin != 0u) {
        return acc * p.w1 + orig[orig_index] * p.w2;
    }
    return acc;
}

@compute @workgroup_size(256)
fn k_gauss_x(@builtin(local_invocation_id) l: vec3<u32>, @builtin(workgroup_id) w: vec3<u32>) {
    let r = p.radius;
    let line_base = p.nx * (w.y + p.ny * w.z);
    let x0 = w.x * TILE_X;
    let width = TILE_X + 2u * r;
    let taps_n = 2u * r + 1u;
    let x = x0 + l.x;
    var boundary = false;
    if (p.fin != 0u) {
        boundary = x == 0u || w.y == 0u || w.z == 0u || x == p.nx - 1u || w.y == p.ny - 1u || w.z == p.nz - 1u;
    }
    for (var k = 0u; k < 3u; k = k + 1u) {
        for (var j = l.x; j < width; j = j + TILE_X) {
            let q = clamp(i32(x0 + j) - i32(r), 0, i32(p.nx) - 1);
            tile_x[j] = src[k * p.n + line_base + u32(q)];
        }
        workgroupBarrier();
        if (x < p.nx) {
            var res = 0.0;
            if (!boundary) {
                var acc = 0.0;
                for (var t = 0u; t < taps_n; t = t + 1u) {
                    acc = acc + taps[p.tap_off + t] * tile_x[l.x + t];
                }
                res = gauss_finish(acc, k * p.n + line_base + x);
            }
            dst[k * p.n + line_base + x] = res;
        }
        workgroupBarrier();
    }
}

@compute @workgroup_size(32, 8, 1)
fn k_gauss_yz(@builtin(local_invocation_id) l: vec3<u32>, @builtin(workgroup_id) w: vec3<u32>) {
    let r = p.radius;
    let x = w.x * 32u + l.x;
    let seg0 = w.y * SEG;
    let line_base = w.z * p.pad0;
    let rows = SEG + 2u * r;
    let taps_n = 2u * r + 1u;
    for (var k = 0u; k < 3u; k = k + 1u) {
        for (var row = l.y; row < rows; row = row + 8u) {
            let a = clamp(i32(seg0 + row) - i32(r), 0, i32(p.len) - 1);
            var v = 0.0;
            if (x < p.nx) {
                v = src[k * p.n + line_base + u32(a) * p.stride + x];
            }
            tile_yz[row * 32u + l.x] = v;
        }
        workgroupBarrier();
        for (var o = l.y; o < SEG; o = o + 8u) {
            let a = seg0 + o;
            if (x < p.nx && a < p.len) {
                let i = line_base + a * p.stride + x;
                var y = a;
                var z = w.z;
                if (p.axis == 2u) {
                    y = w.z;
                    z = a;
                }
                var boundary = false;
                if (p.fin != 0u) {
                    boundary = x == 0u || y == 0u || z == 0u || x == p.nx - 1u || y == p.ny - 1u || z == p.nz - 1u;
                }
                var res = 0.0;
                if (!boundary) {
                    var acc = 0.0;
                    for (var t = 0u; t < taps_n; t = t + 1u) {
                        acc = acc + taps[p.tap_off + t] * tile_yz[(o + t) * 32u + l.x];
                    }
                    res = gauss_finish(acc, k * p.n + i);
                }
                dst[k * p.n + i] = res;
            }
        }
        workgroupBarrier();
    }
}
