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
// Portions derived from ITK `itkMattesMutualInformationImageToImageMetricv4GetValueAndDerivativeThreader.hxx`
// (`ProcessPoint`: joint PDF and fixed marginal accumulation, here in `2^-20` fixed point per
// workgroup). See `docs/THIRD_PARTY_PORTS.md`.

const STRIDE: u32 = 1056u;
const SCALE: f32 = 1048576.0;

@group(0) @binding(2) var<storage, read_write> partials: array<u32>;
@group(0) @binding(3) var<uniform> u: MiU;

var<workgroup> wg_joint: array<atomic<u32>, 1024>;
var<workgroup> wg_marg: array<atomic<u32>, 32>;

@compute @workgroup_size(256)
fn k_mi_hist(@builtin(local_invocation_id) lid3: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>) {
    let lid = lid3.x;
    for (var k = lid; k < 1024u; k = k + 256u) {
        atomicStore(&wg_joint[k], 0u);
    }
    if (lid < 32u) {
        atomicStore(&wg_marg[lid], 0u);
    }
    workgroupBarrier();
    let n = u.dims.w;
    let chunk = u.cfg.y;
    let bins = u.cfg.x;
    let base = wid.x * chunk;
    for (var j = 0u; j < chunk; j = j + 256u) {
        let i = base + j + lid;
        if (i < n) {
            let h = locate(i, u);
            if (h.ok) {
                for (var k = -1; k <= 2; k = k + 1) {
                    let pm = h.mi + k;
                    let arg = f32(pm) - h.term;
                    let q = u32(b3(arg) * SCALE + 0.5);
                    atomicAdd(&wg_joint[h.fbin * bins + u32(pm)], q);
                }
                atomicAdd(&wg_marg[h.fbin], 1u);
            }
        }
    }
    workgroupBarrier();
    let out = wid.x * STRIDE;
    for (var k = lid; k < 1024u; k = k + 256u) {
        partials[out + k] = atomicLoad(&wg_joint[k]);
    }
    if (lid < 32u) {
        partials[out + 1024u + lid] = atomicLoad(&wg_marg[lid]);
    }
}
