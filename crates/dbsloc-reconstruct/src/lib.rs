// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Reconstruction: TRAC/CORE / trajectories. The Python extension lives in `dbsloc-pipeline`.

mod composite;
mod electrode_catalog;
mod kabsch;
mod math;
pub mod metal_axis;
pub mod native_frame;
pub mod orientation;
mod pacer;
mod pacer_fiducials;
mod percentile;
mod plausibility;
mod ring;
pub mod sampling;
mod trac;

pub use composite::{composite_mean_3d, CompositeError};
pub use electrode_catalog::{
    by_name as electrode_by_name, model_names as electrode_model_names, ElectrodeGeometry,
};
pub use kabsch::{apply_rigid_r_rows, estimate_rigid_kabsch, KabschError};
pub use math::{add3, cross3, norm3, scale3, sub3, unit3, unit3_or};
pub use pacer::{
    fit_param_poly_to_skeleton, inv_poly_arc_from, inv_poly_arc_length3, poly_arc_length3,
    polyval3, run_pacer, signed_poly_arc_length3, CtVolume, ElectrodeSpec, PacerConfig,
    PacerElectrode, PacerError, PolyR3,
};
pub use pacer_fiducials::{
    ct_volume_from_ijk, pacer_leads, pacer_sides, side_from_electrode, PacerLead, PacerSide,
};
pub use plausibility::{
    check_in_brain_mask, check_metal_on_axis, check_side, check_tilt, py_float, py_list,
    tilt_from_vertical_deg, PlausibilityLimits, WorldBounds, BRAIN_MASK_MARGIN_MM,
    MAX_TILT_FROM_VERTICAL_DEG, METAL_ON_AXIS_MIN_FRACTION,
};
pub use ring::ring_in_plane;
pub use trac::{
    apex_to_proximal_metal_mm, core_on_line, trace_fiducials_flat, trace_side, TracConfig,
    TracError, TracResult,
};
