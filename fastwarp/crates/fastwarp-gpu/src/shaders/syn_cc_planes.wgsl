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
// (the five per-voxel window planes written to separate buffers instead of one packed array).
// See `docs/THIRD_PARTY_PORTS.md`.

struct P {
    n: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<storage, read> fix: array<f32>;
@group(0) @binding(1) var<storage, read> mov: array<f32>;
@group(0) @binding(2) var<storage, read_write> p_a: array<f32>;
@group(0) @binding(3) var<storage, read_write> p_b: array<f32>;
@group(0) @binding(4) var<storage, read_write> p_aa: array<f32>;
@group(0) @binding(5) var<storage, read_write> p_bb: array<f32>;
@group(0) @binding(6) var<storage, read_write> p_ab: array<f32>;
@group(0) @binding(7) var<uniform> p: P;

@compute @workgroup_size(256)
fn k_cc_planes(
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) w: vec3<u32>,
    @builtin(num_workgroups) nw: vec3<u32>,
) {
    let i = (w.y * nw.x + w.x) * 256u + li;
    if (i >= p.n) {
        return;
    }
    let a = fix[i];
    let b = mov[i];
    p_a[i] = a;
    p_b[i] = b;
    p_aa[i] = a * a;
    p_bb[i] = b * b;
    p_ab[i] = a * b;
}
