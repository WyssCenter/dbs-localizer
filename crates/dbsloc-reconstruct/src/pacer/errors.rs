// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! PaCER pipeline errors — see `specs/10_pacer.md` §10.

use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum PacerError {
    #[error("no electrode artifact found after threshold retries")]
    NoElectrode,
    #[error("CT not usable as stacked planes for skeleton (oblique or degenerate)")]
    ObliqueCt,
    #[error("skeleton empty after filtering")]
    EmptySkeleton,
    #[error("unknown electrode type string: {0}")]
    UnknownElectrode(String),
    #[error("interpolation / OOR produced no valid samples")]
    Interpolation,
    #[error("convex hull build failed: {0}")]
    Hull(String),
    #[error("the brain mask has shape {mask:?} but the CT has shape {ct:?} (k, j, i)")]
    BrainMaskGrid {
        mask: (usize, usize, usize),
        ct: (usize, usize, usize),
    },
    #[error("min eigenvalue in PCA: degenerate point set")]
    DegeneratePca,
    #[error(
        "no distal metal edge in the first 20 mm of the intensity profile; the tip lies \
         before the traced skeleton, so the contacts cannot be placed"
    )]
    NoDistalEdge,
    #[error(
        "calibrated tip apex lies {:.2} mm along the skeleton, before its start; the levels \
         cannot be placed on it",
        *apex_cmm as f64 / 100.0
    )]
    ApexBeforeSkeleton {
        /// Hundredths of a mm, so the error stays comparable by value.
        apex_cmm: i64,
    },
    #[error(
        "the most-proximal level needs {:.2} mm of skeleton but only {:.2} mm was traced",
        *need_cmm as f64 / 100.0,
        *have_cmm as f64 / 100.0
    )]
    LevelsPastSkeleton { need_cmm: i64, have_cmm: i64 },
}

pub type PacerResult<T> = Result<T, PacerError>;
