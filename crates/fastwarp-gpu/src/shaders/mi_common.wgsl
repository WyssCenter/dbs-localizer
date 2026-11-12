// SPDX-FileCopyrightText: 1999-2019 Insight Software Consortium
// SPDX-FileCopyrightText: 2020-present NumFOCUS
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0
//
// Copyright NumFOCUS (Insight Software Consortium)
// Copyright 2026 Wyss Center for Bio and Neuroengineering
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// Portions derived from ITK `itkBSplineKernelFunction.h`, `itkBSplineDerivativeKernelFunction.h`
// (order 3), `itkLinearInterpolateImageFunction.h`, `itkImageFunction.hxx` (`IsInsideBuffer`),
// `itkCentralDifferenceImageFunction.hxx` and the per-point steps of
// `itkMattesMutualInformationImageToImageMetricv4GetValueAndDerivativeThreader.hxx`
// (`ProcessPoint`). See `docs/THIRD_PARTY_PORTS.md`.

struct MiU {
    dims: vec4<u32>,
    cfg: vec4<u32>,
    t0: vec4<f32>,
    t1: vec4<f32>,
    t2: vec4<f32>,
    ox: vec4<f32>,
    oy: vec4<f32>,
    oz: vec4<f32>,
    gscale: vec4<f32>,
    rng: vec4<f32>,
}

struct Hit {
    ok: bool,
    term: f32,
    mi: i32,
    fbin: u32,
    c: vec3<f32>,
    p: vec3<f32>,
}

@group(0) @binding(0) var<storage, read> samples: array<vec4<u32>>;
@group(0) @binding(1) var<storage, read> moving: array<f32>;

fn inside(c: vec3<f32>, nx: u32, ny: u32, nz: u32) -> bool {
    let hi = vec3<f32>(f32(nx) - 0.5, f32(ny) - 0.5, f32(nz) - 0.5);
    return all(c >= vec3<f32>(-0.5)) && all(c < hi);
}

fn tri(c: vec3<f32>, nx: u32, ny: u32, nz: u32) -> f32 {
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
    let b00 = y0 * nx + z0 * sxy;
    let b10 = y1 * nx + z0 * sxy;
    let b01 = y0 * nx + z1 * sxy;
    let b11 = y1 * nx + z1 * sxy;
    let a00 = moving[b00 + x0];
    let a10 = moving[b10 + x0];
    let a01 = moving[b01 + x0];
    let a11 = moving[b11 + x0];
    let c00 = a00 + (moving[b00 + x1] - a00) * t.x;
    let c10 = a10 + (moving[b10 + x1] - a10) * t.x;
    let c01 = a01 + (moving[b01 + x1] - a01) * t.x;
    let c11 = a11 + (moving[b11 + x1] - a11) * t.x;
    let c0 = c00 + (c10 - c00) * t.y;
    let c1 = c01 + (c11 - c01) * t.y;
    return c0 + (c1 - c0) * t.z;
}

fn b3(u: f32) -> f32 {
    let a = abs(u);
    if (a < 1.0) {
        let s = a * a;
        return (4.0 - 6.0 * s + 3.0 * s * a) / 6.0;
    }
    if (a < 2.0) {
        let s = a * a;
        return (8.0 - 12.0 * a + 6.0 * s - s * a) / 6.0;
    }
    return 0.0;
}

fn db3(u: f32) -> f32 {
    if (u >= 0.0 && u < 1.0) {
        return -2.0 * u + 1.5 * u * u;
    }
    if (u > -1.0 && u < 0.0) {
        return -2.0 * u - 1.5 * u * u;
    }
    if (u >= 1.0 && u < 2.0) {
        return -2.0 + 2.0 * u - 0.5 * u * u;
    }
    if (u > -2.0 && u <= -1.0) {
        return 2.0 + 2.0 * u + 0.5 * u * u;
    }
    return 0.0;
}

fn locate(i: u32, u: MiU) -> Hit {
    var h: Hit;
    h.ok = false;
    h.term = 0.0;
    h.mi = 0;
    h.fbin = 0u;
    h.c = vec3<f32>(0.0);
    let s = samples[i];
    let p = bitcast<vec3<f32>>(s.xyz);
    h.p = p;
    let c = vec3<f32>(
        dot(u.t0.xyz, p) + u.t0.w,
        dot(u.t1.xyz, p) + u.t1.w,
        dot(u.t2.xyz, p) + u.t2.w,
    );
    if (!inside(c, u.dims.x, u.dims.y, u.dims.z)) {
        return h;
    }
    let v = tri(c, u.dims.x, u.dims.y, u.dims.z);
    if (v < u.rng.x || v > u.rng.y) {
        return h;
    }
    let term = v / u.rng.z - u.rng.w;
    h.ok = true;
    h.term = term;
    h.c = c;
    h.mi = clamp(i32(term), 2, i32(u.cfg.x) - 3);
    h.fbin = s.w;
    return h;
}

fn axis_gradient(c: vec3<f32>, o: vec3<f32>, scale: f32, u: MiU) -> f32 {
    let a = c - o;
    let b = c + o;
    if (inside(a, u.dims.x, u.dims.y, u.dims.z) && inside(b, u.dims.x, u.dims.y, u.dims.z)) {
        return (tri(b, u.dims.x, u.dims.y, u.dims.z) - tri(a, u.dims.x, u.dims.y, u.dims.z)) * scale;
    }
    return 0.0;
}

fn gradient(h: Hit, u: MiU) -> vec3<f32> {
    return vec3<f32>(
        axis_gradient(h.c, u.ox.xyz, u.gscale.x, u),
        axis_gradient(h.c, u.oy.xyz, u.gscale.y, u),
        axis_gradient(h.c, u.oz.xyz, u.gscale.z, u),
    );
}
