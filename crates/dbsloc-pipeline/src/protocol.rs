// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! JSON stdin/stdout protocol — re-exported from `dbsloc-protocol`.
//!
//! The wire types live in `dbsloc-protocol` now, so the browser client can use them without the
//! pipeline (and its GPU dependencies). The historical path `dbsloc_pipeline::protocol::*` is kept
//! working by this re-export.

pub use dbsloc_protocol::protocol::{emit, RunEvent, StageOutcome};
