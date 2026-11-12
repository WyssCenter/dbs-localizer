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
// (the per-voxel local CC and its per-workgroup partial sum, with the five window-sum planes
// bound separately). See `docs/THIRD_PARTY_PORTS.md`.

struct P {
    nx: u32,
    ny: u32,
    nz: u32,
    n: u32,
    radius: u32,
    part_base: u32,
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
@group(0) @binding(5) var<storage, read_write> part: array<f32>;
@group(0) @binding(6) var<uniform> p: P;

var<workgroup> sh: array<f32, 256>;

fn window_count(pos: u32, n: u32, r: u32) -> f32 {
    var lo = 0u;
    if (pos > r) {
        lo = pos - r;
    }
    let hi = min(pos + r, n - 1u);
    return f32(hi - lo + 1u);
}

@compute @workgroup_size(8, 8, 4)
fn k_cc_value(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) w: vec3<u32>,
    @builtin(num_workgroups) nw: vec3<u32>,
) {
    var val = 0.0;
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
        var cc = 1.0;
        let den = sff * smm;
        if (abs(den) > EPS && sff > REL * sii && smm > REL * sjj) {
            cc = sfm * sfm / den;
        }
        val = -cc;
    }
    sh[li] = val;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (li < s) {
            sh[li] = sh[li] + sh[li + s];
        }
        workgroupBarrier();
    }
    if (li == 0u) {
        part[p.part_base + w.x + nw.x * (w.y + nw.y * w.z)] = sh[0];
    }
}
