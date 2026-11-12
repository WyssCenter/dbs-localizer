// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use thiserror::Error;

/// Errors of the connectome readers.
#[derive(Debug, Error)]
pub enum Error {
    /// A file or folder could not be read.
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The MAT container could not be read or written.
    #[error("{path}: {message}")]
    Mat { path: PathBuf, message: String },
    /// `E_CONN_FORMAT`: the content does not follow the connectome format.
    #[error("E_CONN_FORMAT: {0}")]
    Format(String),
    /// A valid input outside the supported subset.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The read was cancelled by the caller.
    #[error("cancelled")]
    Cancelled,
}

/// Result alias of the connectome readers.
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn format_err(message: impl Into<String>) -> Error {
    Error::Format(message.into())
}

pub(crate) fn io_err(path: &std::path::Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}
