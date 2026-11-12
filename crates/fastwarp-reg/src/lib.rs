// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! Registration engine, CPU foundation: image grids and volumes in ITK conventions,
//! ITK / ANTs-compatible transform I/O, and point mapping.
//!
//! Module map:
//!
//! * `grid`, `volume`, `nifti_io`: [`Grid`], [`Volume`], NIfTI geometry as ITK reads it.
//! * `transform`: affine, displacement field and composite transforms, `.mat`, `.txt`, `.h5` and
//!   displacement-field NIfTI files and ANTs composite pairs.
//! * `points`: mapping points through a chain.
//!
//! * `preprocess`, `pyramid`, `sampling`, `convergence`, `interp`, `metric`: the numerics of an
//!   `antsRegistration` linear or deformable stage in `f64` (winsorise / rescale, shrunk virtual
//!   domains and recursive Gaussian smoothing, Mersenne Twister sample selection, window
//!   convergence monitor, ANTs neighbourhood CC and Mattes MI with derivatives). They are the
//!   executable contract a GPU implementation must match.
//!
//! * `backend`, `cpu`, `field_ops`, `syn`, `recipe`, `apply`: the backend trait the loops run on,
//!   its `f64` CPU reference, the displacement-field operations of ITK's SyN, the SyN loop itself,
//!   the cohort SyN recipe as data, and `antsApplyTransforms` on the CPU.
//!
//! * `init`, `optimizer`, `linear`: centre-of-mass initialisation, parameter scales and ITK's
//!   conjugate-gradient line-search optimiser, and the `Rigid` / `Affine` Mattes MI stages over a
//!   metric engine (CPU reference here, GPU in `fastwarp-gpu`).

pub mod apply;
pub mod backend;
pub mod convergence;
pub mod cpu;
pub mod error;
pub mod field_ops;
pub mod grid;
pub mod init;
pub mod interp;
pub mod linalg;
pub mod linear;
pub mod mask;
pub mod matv73;
pub mod metric;
pub mod n4;
pub mod nifti_io;
pub mod optimizer;
pub mod points;
pub mod preprocess;
pub mod pyramid;
pub mod recipe;
pub mod register;
pub mod sampling;
pub mod syn;
pub mod transform;
pub mod volume;

pub use backend::Backend;
pub use cpu::CpuBackend;
pub use error::{RegError, Result};
pub use field_ops::VectorField;
pub use grid::{lps_to_ras, ras_to_lps, Grid};
pub use linalg::{Mat3, Vec3};
pub use mask::MaskVolume;
pub use points::{map_point_ras, map_points_ras, map_points_ras_lattice};
pub use transform::{
    AffineTransform, CompositePair, DisplacementField, TransformChain, TransformElem,
};
pub use volume::Volume;
