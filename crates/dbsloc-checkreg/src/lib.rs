// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

pub mod error;
pub mod nifti_io;

#[cfg(feature = "pair")]
pub mod checkreg_pair;

pub use error::{CheckregError, CheckregResult};

#[cfg(feature = "pair")]
pub use checkreg_pair::{
    crosshair_uv_for_plane, gencheckregpair_png, load_prepared_checkreg_volumes,
    load_prepared_checkreg_volumes_cancellable, pick_ijk_from_plane_uv, CheckregSlicePlane,
    GencheckregpairOptions, PreparedCheckregVols, RawWindowStats, SliceLayers,
};
