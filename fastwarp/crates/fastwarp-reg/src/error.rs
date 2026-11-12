// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;

/// Errors of the registration crate.
#[derive(Debug, Error)]
pub enum RegError {
    /// A file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The `nifti` crate rejected a file.
    #[error("NIfTI {path}: {message}")]
    Nifti { path: PathBuf, message: String },
    /// A NIfTI header declares more voxels per axis than `MAX_DIM`.
    #[error("NIfTI {path}: dims too large: {dims:?}")]
    NiftiDimsTooLarge { path: PathBuf, dims: Vec<usize> },
    /// A NIfTI header whose geometry or layout ITK would refuse.
    #[error("NIfTI {path}: {message}")]
    NiftiGeometry { path: PathBuf, message: String },
    /// A grid with a non-positive spacing, a singular direction or a zero size.
    #[error("invalid grid: {0}")]
    InvalidGrid(String),
    /// A file that does not follow its format.
    #[error("{format}: {message}")]
    Format {
        format: &'static str,
        message: String,
    },
    /// A valid file that uses a feature this crate does not implement.
    #[error("{format}: unsupported feature: {feature}")]
    Unsupported {
        format: &'static str,
        feature: String,
    },
    /// A buffer whose length does not match its grid or declared shape.
    #[error("{what}: expected {expected} value(s), got {got}")]
    Length {
        what: &'static str,
        expected: usize,
        got: usize,
    },
    /// A transform or chain that has no inverse (singular matrix, or a displacement field).
    #[error("not invertible: {0}")]
    NotInvertible(String),
    /// An input a preprocessing step, sampler or metric cannot work with.
    #[error("{what}: {message}")]
    Invalid { what: &'static str, message: String },
    #[error("cancelled")]
    Cancelled,
}

/// Result alias of the registration crate.
pub type Result<T> = std::result::Result<T, RegError>;

impl From<fastwarp_matfile::MatError> for RegError {
    fn from(e: fastwarp_matfile::MatError) -> Self {
        match e {
            fastwarp_matfile::MatError::Io { path, source } => RegError::Io { path, source },
            fastwarp_matfile::MatError::Format(message) => RegError::Format {
                format: "MATLAB v7.3",
                message,
            },
            fastwarp_matfile::MatError::Unsupported(feature) => RegError::Unsupported {
                format: "MATLAB v7.3",
                feature,
            },
        }
    }
}

pub(crate) fn fmt_err(format: &'static str, message: impl Into<String>) -> RegError {
    RegError::Format {
        format,
        message: message.into(),
    }
}

pub(crate) fn unsupported(format: &'static str, feature: impl Into<String>) -> RegError {
    RegError::Unsupported {
        format,
        feature: feature.into(),
    }
}

pub(crate) fn invalid(what: &'static str, message: impl Into<String>) -> RegError {
    RegError::Invalid {
        what,
        message: message.into(),
    }
}

pub(crate) fn check_cancel(cancel: Option<&AtomicBool>) -> Result<()> {
    if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
        Err(RegError::Cancelled)
    } else {
        Ok(())
    }
}
