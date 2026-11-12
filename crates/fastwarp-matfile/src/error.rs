// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use thiserror::Error;

/// Errors of the MATLAB file reader.
#[derive(Debug, Error)]
pub enum MatError {
    /// The file could not be opened or read.
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file does not follow the HDF5 / MATLAB v7.3 format.
    #[error("MATLAB v7.3: {0}")]
    Format(String),
    /// A valid file that uses a feature this reader does not implement.
    #[error("MATLAB v7.3: unsupported feature: {0}")]
    Unsupported(String),
}

/// Result alias of the MATLAB file reader.
pub type Result<T> = std::result::Result<T, MatError>;

pub(crate) fn fmt_err(message: impl Into<String>) -> MatError {
    MatError::Format(message.into())
}

pub(crate) fn unsupported(feature: impl Into<String>) -> MatError {
    MatError::Unsupported(feature.into())
}
