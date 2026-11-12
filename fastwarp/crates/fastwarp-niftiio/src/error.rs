// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use nifti::NiftiType;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NiftiFileError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Header(String),
    #[error("the file only holds a header (no volume data)")]
    NoVolumeData,
    #[error("volume size overflows")]
    BadVolumeSize,
    #[error("cannot reserve {0} byte(s) for the volume")]
    Reserve(usize),
    #[error("incompatible volume length: found {got} byte(s), expected {want}")]
    IncompatibleLength { got: usize, want: usize },
    #[error("unsupported data type {0:?}")]
    UnsupportedDataType(NiftiType),
    #[error("{0}")]
    Write(String),
}
