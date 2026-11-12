// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

struct BoxParams {
    nx: u32,
    ny: u32,
    nz: u32,
    n: u32,
    axis_len: u32,
    axis_stride: u32,
    other_stride: u32,
    radius: u32,
    planes: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

const TILE: u32 = 256u;
const XW: u32 = 272u;
const SEG: u32 = 32u;

@group(0) @binding(0) var<storage, read> src: array<f32>;
@group(0) @binding(1) var<storage, read_write> dst: array<f32>;
@group(0) @binding(2) var<uniform> b: BoxParams;

var<workgroup> tile_x: array<f32, 1632>;
var<workgroup> tile_yz: array<f32, 1536>;

@compute @workgroup_size(256)
fn k_box_x(@builtin(local_invocation_id) l: vec3<u32>, @builtin(workgroup_id) w: vec3<u32>) {
    let r = b.radius;
    let line_base = b.nx * (w.y + b.ny * w.z);
    let i0 = w.x * TILE;
    let width = TILE + 2u * r;
    for (var p = 0u; p < b.planes; p = p + 1u) {
        let pb = p * b.n + line_base;
        for (var k = l.x; k < width; k = k + TILE) {
            let i = i32(i0 + k) - i32(r);
            var v = 0.0;
            if (i >= 0 && i < i32(b.nx)) {
                v = src[pb + u32(i)];
            }
            tile_x[p * XW + k] = v;
        }
    }
    workgroupBarrier();
    let i = i0 + l.x;
    if (i < b.nx) {
        for (var p = 0u; p < b.planes; p = p + 1u) {
            var s = 0.0;
            for (var k = 0u; k <= 2u * r; k = k + 1u) {
                s = s + tile_x[p * XW + l.x + k];
            }
            dst[p * b.n + line_base + i] = s;
        }
    }
}

@compute @workgroup_size(32, 8, 1)
fn k_box_yz(@builtin(local_invocation_id) l: vec3<u32>, @builtin(workgroup_id) w: vec3<u32>) {
    let r = b.radius;
    let x = w.x * 32u + l.x;
    let seg0 = w.y * SEG;
    let line_base = w.z * b.other_stride;
    let rows = SEG + 2u * r;
    for (var p = 0u; p < b.planes; p = p + 1u) {
        let pb = p * b.n + line_base;
        for (var row = l.y; row < rows; row = row + 8u) {
            let a = i32(seg0 + row) - i32(r);
            var v = 0.0;
            if (x < b.nx && a >= 0 && a < i32(b.axis_len)) {
                v = src[pb + u32(a) * b.axis_stride + x];
            }
            tile_yz[row * 32u + l.x] = v;
        }
        workgroupBarrier();
        for (var o = l.y; o < SEG; o = o + 8u) {
            let a = seg0 + o;
            if (x < b.nx && a < b.axis_len) {
                var s = 0.0;
                for (var k = 0u; k <= 2u * r; k = k + 1u) {
                    s = s + tile_yz[(o + k) * 32u + l.x];
                }
                dst[pb + a * b.axis_stride + x] = s;
            }
        }
        workgroupBarrier();
    }
}
