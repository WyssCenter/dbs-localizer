// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Per-subject connectome maps: structural and functional seed maps, per-fibre values and the
//! connectivity job of `specs/15` §5 to §9.

pub mod fibervals;
pub mod functional;
pub mod job;
pub mod nii;
pub mod source;
pub mod structural;

use std::sync::atomic::{AtomicBool, Ordering};

pub use nii::{Affine, NiiVolume};

/// Errors of the connectivity maps; [`MapError::code`] gives the `specs/15` §11 error code.
#[derive(Debug, Clone, PartialEq)]
pub enum MapError {
    Cancelled,
    SeedEmpty(String),
    Format(String),
    Missing(String),
    Space(String),
    Usage(String),
    Io(String),
    SubsetTooLarge(String),
    Memory(String),
}

impl MapError {
    /// The error code of `specs/15` §11 (`cancelled` for a cancel).
    pub fn code(&self) -> &'static str {
        match self {
            MapError::Cancelled => "cancelled",
            MapError::SeedEmpty(_) => "E_SEED_EMPTY",
            MapError::Format(_) => "E_CONN_FORMAT",
            MapError::Missing(_) => "E_CONN_MISSING",
            MapError::Space(_) => "E_CONN_SPACE",
            MapError::Usage(_) => "E_CONN_USAGE",
            MapError::Io(_) => "E_CONN_IO",
            MapError::SubsetTooLarge(_) => "E_CONN_SUBSET_TOO_LARGE",
            MapError::Memory(_) => "E_CONN_MEMORY",
        }
    }

    /// The message without the code.
    pub fn message(&self) -> String {
        match self {
            MapError::Cancelled => "cancelled".to_owned(),
            MapError::SeedEmpty(m)
            | MapError::Format(m)
            | MapError::Missing(m)
            | MapError::Space(m)
            | MapError::Usage(m)
            | MapError::Io(m)
            | MapError::SubsetTooLarge(m)
            | MapError::Memory(m) => m.clone(),
        }
    }
}

impl std::fmt::Display for MapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MapError::Cancelled => f.write_str("cancelled"),
            _ => write!(f, "{}: {}", self.code(), self.message()),
        }
    }
}

impl std::error::Error for MapError {}

impl From<dbsloc_connectome::Error> for MapError {
    fn from(e: dbsloc_connectome::Error) -> Self {
        match e {
            dbsloc_connectome::Error::Format(m) => MapError::Format(m),
            dbsloc_connectome::Error::Cancelled => MapError::Cancelled,
            other => MapError::Io(other.to_string()),
        }
    }
}

/// Result alias of this module.
pub type MapResult<T> = Result<T, MapError>;

/// One progress report: what is running and how far it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub stage: String,
    pub done: u64,
    pub total: u64,
    pub message: String,
}

/// Progress sink.
pub type ProgressFn<'a> = &'a (dyn Fn(&Progress) + Sync);

pub(crate) fn check_cancel(cancel: &AtomicBool) -> MapResult<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(MapError::Cancelled)
    } else {
        Ok(())
    }
}

pub(crate) fn check_cancel_opt(cancel: Option<&AtomicBool>) -> MapResult<()> {
    cancel.map_or(Ok(()), check_cancel)
}

pub(crate) fn report(progress: ProgressFn<'_>, stage: &str, done: u64, total: u64, msg: &str) {
    progress(&Progress {
        stage: stage.to_owned(),
        done,
        total,
        message: msg.to_owned(),
    });
}

/// A progress sink that ignores every report.
pub fn no_progress(_: &Progress) {}
