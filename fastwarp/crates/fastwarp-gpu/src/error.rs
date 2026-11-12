// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

/// Errors raised by the wgpu device layer and the probes.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no usable GPU adapter: {0}")]
    NoAdapter(String),
    #[error("insufficient limit {limit}: have {have}, need {need}")]
    LimitsInsufficient {
        limit: &'static str,
        have: u64,
        need: u64,
    },
    #[error("device request failed: {0}")]
    RequestDevice(String),
    #[error("out of memory allocating {requested} bytes for {label}")]
    OutOfMemory { requested: u64, label: String },
    #[error("GPU validation error: {0}")]
    Validation(String),
    #[error("GPU internal error: {0}")]
    Internal(String),
    #[error("device lost: {0}")]
    DeviceLost(String),
    #[error("readback failed: {0}")]
    Readback(String),
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("verification failed: {0}")]
    Verification(String),
    #[error("cancelled")]
    Cancelled,
}

/// Result alias for this crate.
pub type GpuResult<T> = Result<T, GpuError>;
