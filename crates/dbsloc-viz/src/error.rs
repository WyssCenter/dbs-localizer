// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Errors from GPU / render initialization (no `wgpu` types here — works with `--no-default-features`).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum VizError {
    #[error("no WGPU / WebGPU adapter (install GPU drivers, or on CI set WGPU_FORCE_FALLBACK_ADAPTER=1)")]
    NoAdapter,
    #[error("WGPU request_device: {0}")]
    RequestDevice(String),
    #[error("WGPU: {0}")]
    Wgpu(String),
    #[error("headless readback: {0}")]
    Readback(String),
    /// NIfTI-1 load error (data-mni feature).
    #[error("NIfTI load: {0}")]
    NiftiLoad(String),
    /// Reconstruction JSON parse error (data-mni feature).
    #[error("reconstruction JSON: {0}")]
    Json(String),
    /// Checkreg pair: volume shapes or grid mismatch.
    #[error("checkreg: shape mismatch (moving {moving:?} vs fixed {fixed:?})")]
    CheckregShape {
        moving: Vec<usize>,
        fixed: Vec<usize>,
    },
    /// Checkreg: encode / I/O.
    #[error("checkreg: {0}")]
    Checkreg(String),
}

pub type VizResult<T> = Result<T, VizError>;

#[cfg(feature = "data-mni")]
impl From<dbsloc_checkreg::CheckregError> for VizError {
    fn from(e: dbsloc_checkreg::CheckregError) -> Self {
        match e {
            dbsloc_checkreg::CheckregError::NiftiLoad(m) => Self::NiftiLoad(m),
            dbsloc_checkreg::CheckregError::CheckregShape { moving, fixed } => {
                Self::CheckregShape { moving, fixed }
            }
            dbsloc_checkreg::CheckregError::Checkreg(m) => Self::Checkreg(m),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::VizError;

    #[test]
    fn no_adapter_display() {
        assert!(!VizError::NoAdapter.to_string().is_empty());
    }
}
