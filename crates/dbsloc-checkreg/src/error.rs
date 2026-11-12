// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CheckregError {
    #[error("NIfTI load: {0}")]
    NiftiLoad(String),
    #[error("checkreg: shape mismatch (moving {moving:?} vs fixed {fixed:?})")]
    CheckregShape {
        moving: Vec<usize>,
        fixed: Vec<usize>,
    },
    #[error("checkreg: {0}")]
    Checkreg(String),
}

pub type CheckregResult<T> = Result<T, CheckregError>;
