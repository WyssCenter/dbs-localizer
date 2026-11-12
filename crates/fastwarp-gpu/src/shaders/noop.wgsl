// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

@group(0) @binding(0) var<storage, read_write> counter: array<u32>;

@compute @workgroup_size(64)
fn k_noop(@builtin(local_invocation_index) li: u32) {
    if (li == 0u) {
        counter[0] = counter[0] + 1u;
    }
}
