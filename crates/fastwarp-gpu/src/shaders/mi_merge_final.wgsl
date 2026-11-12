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
// Portions derived from ITK `itkMattesMutualInformationImageToImageMetricv4.hxx`
// (`GetValueCommonAfterThreadedExecution`: merge of the per-thread joint PDFs, second level). See
// `docs/THIRD_PARTY_PORTS.md`.

const STRIDE: u32 = 1056u;

struct MergeU {
    nwg: u32,
    per_stripe: u32,
    stripes: u32,
    pad: u32,
}

@group(0) @binding(0) var<storage, read> stripe_sums: array<u32>;
@group(0) @binding(1) var<storage, read_write> merged: array<u32>;
@group(0) @binding(2) var<uniform> mu: MergeU;

@compute @workgroup_size(256)
fn k_mi_merge_final(@builtin(local_invocation_id) lid3: vec3<u32>) {
    for (var cell = lid3.x; cell < STRIDE; cell = cell + 256u) {
        var lo = 0u;
        var hi = 0u;
        for (var s = 0u; s < mu.stripes; s = s + 1u) {
            let l = stripe_sums[2u * (s * STRIDE + cell)];
            let nl = lo + l;
            if (nl < lo) {
                hi = hi + 1u;
            }
            lo = nl;
            hi = hi + stripe_sums[2u * (s * STRIDE + cell) + 1u];
        }
        merged[2u * cell] = lo;
        merged[2u * cell + 1u] = hi;
    }
}
