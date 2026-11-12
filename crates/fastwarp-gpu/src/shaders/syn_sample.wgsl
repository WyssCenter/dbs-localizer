// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

fn tri(c: vec3<f32>, off: u32, nx: u32, ny: u32, nz: u32) -> f32 {
    let cc = clamp(c, vec3<f32>(0.0), vec3<f32>(f32(nx - 1u), f32(ny - 1u), f32(nz - 1u)));
    let f = floor(cc);
    let t = cc - f;
    let x0 = u32(f.x);
    let y0 = u32(f.y);
    let z0 = u32(f.z);
    let x1 = min(x0 + 1u, nx - 1u);
    let y1 = min(y0 + 1u, ny - 1u);
    let z1 = min(z0 + 1u, nz - 1u);
    let sxy = nx * ny;
    let b00 = off + y0 * nx + z0 * sxy;
    let b10 = off + y1 * nx + z0 * sxy;
    let b01 = off + y0 * nx + z1 * sxy;
    let b11 = off + y1 * nx + z1 * sxy;
    let a00 = src[b00 + x0];
    let a10 = src[b10 + x0];
    let a01 = src[b01 + x0];
    let a11 = src[b11 + x0];
    let c00 = a00 + (src[b00 + x1] - a00) * t.x;
    let c10 = a10 + (src[b10 + x1] - a10) * t.x;
    let c01 = a01 + (src[b01 + x1] - a01) * t.x;
    let c11 = a11 + (src[b11 + x1] - a11) * t.x;
    let c0 = c00 + (c10 - c00) * t.y;
    let c1 = c01 + (c11 - c01) * t.y;
    return c0 + (c1 - c0) * t.z;
}

fn inside(c: vec3<f32>, nx: u32, ny: u32, nz: u32) -> bool {
    let hi = vec3<f32>(f32(nx) - 0.5, f32(ny) - 0.5, f32(nz) - 0.5);
    return all(c >= vec3<f32>(-0.5)) && all(c < hi);
}
