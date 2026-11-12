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
// (`ProcessPoint`) and `itkMattesMutualInformationImageToImageMetricv4.hxx` (`ComputeResults`):
// the derivative contribution of one sample, with the `pRatio` factor applied per sample instead of
// per histogram cell. See `docs/THIRD_PARTY_PORTS.md`.

@group(0) @binding(2) var<storage, read> ratio: array<f32>;
@group(0) @binding(3) var<storage, read> gk: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> dpart: array<f32>;
@group(0) @binding(5) var<uniform> u: MiU;

var<workgroup> red: array<f32, 3072>;

@compute @workgroup_size(256)
fn k_mi_deriv(@builtin(local_invocation_id) lid3: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>) {
    let lid = lid3.x;
    let n = u.dims.w;
    let chunk = u.cfg.y;
    let bins = u.cfg.x;
    let np = u.cfg.z;
    let base = wid.x * chunk;
    var acc: array<f32, 12>;
    for (var k = 0u; k < 12u; k = k + 1u) {
        acc[k] = 0.0;
    }
    for (var j = 0u; j < chunk; j = j + 256u) {
        let i = base + j + lid;
        if (i < n) {
            let h = locate(i, u);
            if (h.ok) {
                let g = gradient(h, u);
                var s = 0.0;
                for (var k = -1; k <= 2; k = k + 1) {
                    let pm = h.mi + k;
                    let arg = f32(pm) - h.term;
                    s = s + db3(arg) * ratio[h.fbin * bins + u32(pm)];
                }
                let pp = vec4<f32>(h.p, 1.0);
                for (var q = 0u; q < np; q = q + 1u) {
                    let jv = vec3<f32>(
                        dot(gk[3u * q], pp),
                        dot(gk[3u * q + 1u], pp),
                        dot(gk[3u * q + 2u], pp),
                    );
                    acc[q] = acc[q] + s * dot(g, jv);
                }
            }
        }
    }
    for (var q = 0u; q < 12u; q = q + 1u) {
        red[q * 256u + lid] = acc[q];
    }
    workgroupBarrier();
    for (var stride = 128u; stride > 0u; stride = stride >> 1u) {
        if (lid < stride) {
            for (var q = 0u; q < 12u; q = q + 1u) {
                red[q * 256u + lid] = red[q * 256u + lid] + red[q * 256u + lid + stride];
            }
        }
        workgroupBarrier();
    }
    if (lid < 12u) {
        dpart[wid.x * 12u + lid] = red[lid * 256u];
    }
}
