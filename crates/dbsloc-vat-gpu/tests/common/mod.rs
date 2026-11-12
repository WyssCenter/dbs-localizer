// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use fastwarp_gpu::{BackendSelect, GpuError, WgpuBackend};

pub fn require_adapter() -> bool {
    std::env::var("DBSLOC_GPU_REQUIRE_ADAPTER")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

pub fn backend() -> Option<WgpuBackend> {
    match WgpuBackend::new(&BackendSelect::from_env().unwrap_or_default()) {
        Ok(b) => {
            eprintln!(
                "adapter: {} ({}), selftest attempts {}",
                b.context().info.name,
                b.context().info.backend,
                b.selftest_record().attempts
            );
            Some(b)
        }
        Err(GpuError::NoAdapter(why)) => {
            if require_adapter() {
                panic!("no GPU adapter with DBSLOC_GPU_REQUIRE_ADAPTER set: {why}");
            }
            eprintln!("skip: no adapter ({why})");
            None
        }
        Err(e) => panic!("backend creation failed: {e}"),
    }
}
