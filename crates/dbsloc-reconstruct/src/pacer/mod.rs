// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! PaCER-equivalent pipeline (clean-room; see `specs/10_pacer.md`).

mod catalog;
mod errors;
mod mask;
mod metal;
mod midline;
mod oor;
mod peaks;
mod pipeline;
mod poly3;
mod skeleton_pass1;
mod volume;

pub use catalog::{by_name as catalog_by_name, ElectrodeSpec};
pub use errors::PacerError;
pub(crate) use pipeline::run_pacer_per_cloud;
pub use pipeline::{run_pacer, PacerConfig, PacerElectrode};
pub use poly3::{
    fit_param_poly_to_skeleton, inv_poly_arc_from, inv_poly_arc_length3, poly_arc_length3,
    polyval3, signed_poly_arc_length3, PolyR3,
};
pub use volume::CtVolume;
