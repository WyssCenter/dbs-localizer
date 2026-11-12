// SPDX-FileCopyrightText: 1999-2019 Insight Software Consortium
// SPDX-FileCopyrightText: 2020-present NumFOCUS
// SPDX-FileCopyrightText: ANTs developers
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
// Portions derived from ITK
// `itkANTSNeighborhoodCorrelationImageToImageMetricv4GetValueAndDerivativeThreader.hxx`
// (the local-support derivative, with the five window-sum planes bound separately). See
// `docs/THIRD_PARTY_PORTS.md`.

struct P {
    nx: u32,
    ny: u32,
    nz: u32,
    n: u32,
    radius: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
    o0: vec4<f32>,
    o1: vec4<f32>,
    o2: vec4<f32>,
    sp: vec4<f32>,
}

const REL: f32 = 1e-6;
const EPS: f32 = 2.220446e-16;

@group(0) @binding(0) var<storage, read> s_a: array<f32>;
@group(0) @binding(1) var<storage, read> s_b: array<f32>;
@group(0) @binding(2) var<storage, read> s_aa: array<f32>;
@group(0) @binding(3) var<storage, read> s_bb: array<f32>;
@group(0) @binding(4) var<storage, read> s_ab: array<f32>;
@group(0) @binding(5) var<storage, read> fix: array<f32>;
@group(0) @binding(6) var<storage, read> src: array<f32>;
@group(0) @binding(7) var<storage, read_write> dst: array<f32>;
@group(0) @binding(8) var<uniform> p: P;

fn window_count(pos: u32, n: u32, r: u32) -> f32 {
    var lo = 0u;
    if (pos > r) {
        lo = pos - r;
    }
    let hi = min(pos + r, n - 1u);
    return f32(hi - lo + 1u);
}

fn probe_gradient(c: vec3<f32>, o: vec3<f32>, spacing: f32) -> f32 {
    let cp = c + o;
    let cm = c - o;
    if (inside(cp, p.nx, p.ny, p.nz) && inside(cm, p.nx, p.ny, p.nz)) {
        return (tri(cp, 0u, p.nx, p.ny, p.nz) - tri(cm, 0u, p.nx, p.ny, p.nz)) / spacing;
    }
    return 0.0;
}

@compute @workgroup_size(8, 8, 4)
fn k_cc_deriv(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x < p.nx && id.y < p.ny && id.z < p.nz) {
        let i = id.x + p.nx * (id.y + p.ny * id.z);
        let cn = window_count(id.x, p.nx, p.radius) * window_count(id.y, p.ny, p.radius) * window_count(id.z, p.nz, p.radius);
        let si = s_a[i];
        let sj = s_b[i];
        let sii = s_aa[i];
        let sjj = s_bb[i];
        let sij = s_ab[i];
        let sff = sii - si * si / cn;
        let smm = sjj - sj * sj / cn;
        let sfm = sij - si * sj / cn;
        var d = vec3<f32>(0.0);
        let den = sff * smm;
        if (sff > REL * sii && smm > REL * sjj && sff > EPS && smm > EPS) {
            let fi = fix[i] - si / cn;
            let mi = src[i] - sj / cn;
            let coeff = 2.0 * sfm / den * (fi - sfm / smm * mi);
            let c = vec3<f32>(id);
            let g = vec3<f32>(
                probe_gradient(c, p.o0.xyz, p.sp.x),
                probe_gradient(c, p.o1.xyz, p.sp.y),
                probe_gradient(c, p.o2.xyz, p.sp.z),
            );
            d = coeff * g;
        }
        dst[i] = d.x;
        dst[p.n + i] = d.y;
        dst[2u * p.n + i] = d.z;
    }
}
