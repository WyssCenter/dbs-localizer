// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Mirrored (flipped) stimulation volumes (`specs/14` §4.4): the plain left-right flip about
//! world x = 0 and its combination with the non-linear flip transform of [`super::fliplr`].

use std::path::{Path, PathBuf};

use fastwarp_core::Mat4;
use fastwarp_reg::apply::{apply_chain, Interpolation};
use fastwarp_reg::transform::read_transform_file;
use fastwarp_reg::{Grid, Volume};

use super::fliplr::inverse_of;
use crate::nifti_io::{load_volume_f64, read_grid};
use crate::point_warp::{PointWarp, RustPointWarp};
use crate::vat::outputs::{write_binary_nii, write_f32_nii, Codes};
use crate::vat::settings::Side;

/// Why a volume could not be flipped.
#[derive(Debug, thiserror::Error)]
pub enum FlipError {
    /// The requested flip is not available in this release.
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Io(String),
}

/// What the voxel values mean, which decides how they are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Binary,
    Float,
}

/// The kinds of stimulation image of a label folder and their file-name part.
pub const KINDS: [(ImageKind, &str); 3] = [
    (ImageKind::Binary, "binary"),
    (ImageKind::Float, "efield"),
    (ImageKind::Float, "efieldgauss"),
];

const ALIGN_TOLERANCE: f64 = 1e-9;

fn x_aligned(aff: &Mat4) -> bool {
    aff[0][1].abs() < ALIGN_TOLERANCE
        && aff[0][2].abs() < ALIGN_TOLERANCE
        && aff[1][0].abs() < ALIGN_TOLERANCE
        && aff[2][0].abs() < ALIGN_TOLERANCE
        && aff[0][0].abs() > ALIGN_TOLERANCE
}

/// The affine [`flip_x`] gives the mirrored grid.
pub fn mirrored_affine(dims: [usize; 3], aff: &Mat4) -> Mat4 {
    let mut out = *aff;
    if x_aligned(aff) {
        out[0][3] = -(aff[0][0] * (dims[0] as f64 - 1.0) + aff[0][3]);
    } else {
        for c in 0..4 {
            out[0][c] = -aff[0][c];
        }
    }
    out
}

/// Mirrors a volume about world x = 0 (`specs/14` §4.4: negate x of the affine). When the voxel i axis is the world x axis the data are reversed along i so that the affine keeps its orientation; otherwise only the affine changes. Data are indexed `i + nx * (j + ny * k)`.
pub fn flip_x<T: Copy>(data: &[T], dims: [usize; 3], aff: &Mat4) -> (Vec<T>, Mat4) {
    let [nx, ny, nz] = dims;
    let out_aff = mirrored_affine(dims, aff);
    if !x_aligned(aff) {
        return (data.to_vec(), out_aff);
    }
    let mut out = Vec::with_capacity(data.len());
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                out.push(data[(nx - 1 - i) + nx * (j + ny * k)]);
            }
        }
    }
    (out, out_aff)
}

/// Points of the flipped space with only x negated: the documented fallback of `specs/14` §4.4 when no flip transform is available.
pub fn flip_points(points: &[[f64; 3]]) -> Vec<[f64; 3]> {
    points.iter().map(|p| [-p[0], p[1], p[2]]).collect()
}

/// Points of the flipped space (`specs/14` §4.4, §5.4): x is negated, then the points are mapped through the inverse composite that belongs to the forward composite `transform`.
pub fn flip_points_through(
    points: &[[f64; 3]],
    transform: &Path,
) -> Result<Vec<[f64; 3]>, FlipError> {
    let inverse = inverse_of(transform)
        .filter(|p| p.is_file())
        .ok_or_else(|| {
            FlipError::Unsupported(format!(
                "no inverse composite belongs to {}; points need the from-<space>Flipped_to-<space> file beside it",
                transform.display()
            ))
        })?;
    RustPointWarp::from_file(&inverse)
        .and_then(|w| w.to_template(&flip_points(points)))
        .map_err(|e| FlipError::Io(e.to_string()))
}

fn grid_of(dims: [usize; 3], aff: &Mat4) -> Result<Grid, FlipError> {
    Grid::from_ras_affine(dims, aff).map_err(|e| FlipError::Io(e.to_string()))
}

fn through_transform(
    data: &[f64],
    dims: [usize; 3],
    mirrored_aff: &Mat4,
    kind: ImageKind,
    transform: &Path,
) -> Result<Vec<f64>, FlipError> {
    let chain = read_transform_file(transform)
        .map_err(|e| FlipError::Io(format!("{}: {e}", transform.display())))?;
    let grid = grid_of(dims, mirrored_aff)?;
    let image = Volume::from_vec(grid, data.to_vec()).map_err(|e| FlipError::Io(e.to_string()))?;
    let interpolation = match kind {
        ImageKind::Binary => Interpolation::NearestNeighbor,
        ImageKind::Float => Interpolation::Linear,
    };
    let out = apply_chain(&image, &grid, &chain, interpolation, 0.0)
        .map_err(|e| FlipError::Io(e.to_string()))?;
    Ok(out.data().to_vec())
}

/// Flips a NIfTI image and writes it uncompressed to `dst` on the mirrored grid of `src` (the grid of `src` with x negated, `specs/14` §4.4). Without `transform` only x is mirrored (the header is rewritten, `specs/14` §4.4 fallback); with the forward composite of [`super::fliplr`] the mirrored image is then resampled through it onto that same mirrored grid (nearest neighbour for binary images, linear otherwise).
pub fn flip_nifti(
    src: &Path,
    dst: &Path,
    kind: ImageKind,
    transform: Option<&Path>,
) -> Result<(), FlipError> {
    let (vol, aff) = load_volume_f64(src).map_err(|e| FlipError::Io(e.to_string()))?;
    let (nx, ny, nz) = vol.dim();
    let mut flat = Vec::with_capacity(nx * ny * nz);
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                flat.push(vol[[i, j, k]]);
            }
        }
    }
    let (mirrored, out_aff) = flip_x(&flat, [nx, ny, nz], &aff);
    let data = match transform {
        Some(t) => through_transform(&mirrored, [nx, ny, nz], &out_aff, kind, t)?,
        None => mirrored,
    };
    let codes = Codes::of_image(Some(src));
    let written = match kind {
        ImageKind::Binary => {
            let bytes: Vec<u8> = data.iter().map(|&v| u8::from(v > 0.5)).collect();
            write_binary_nii(dst, [nx, ny, nz], &bytes, &out_aff, codes)
        }
        ImageKind::Float => {
            let floats: Vec<f32> = data.iter().map(|&v| v as f32).collect();
            write_f32_nii(dst, [nx, ny, nz], &floats, &out_aff, codes)
        }
    };
    written.map_err(|e| FlipError::Io(e.to_string()))
}

const CURRENT_TOLERANCE: f64 = 1e-5;

/// Whether the header of `flipped` puts it on the mirrored grid of `original` (`specs/14` §4.4).
pub fn flipped_is_current(original: &Path, flipped: &Path) -> bool {
    let (Ok(o), Ok(f)) = (read_grid(original), read_grid(flipped)) else {
        return false;
    };
    if o.dims != f.dims {
        return false;
    }
    let want = mirrored_affine(o.dims, &o.affine);
    (0..3).all(|r| {
        (0..4).all(|c| {
            let (a, b) = (want[r][c], f.affine[r][c]);
            (a - b).abs() <= CURRENT_TOLERANCE * a.abs().max(b.abs()).max(1.0)
        })
    })
}

/// `sub-<id>_sim-<kind>_model-<slug>_hemi-<R|L>.nii`, the name of an original image.
pub fn original_name(subject: &str, kind: &str, slug: &str, side: Side) -> String {
    format!(
        "sub-{subject}_sim-{kind}_model-{slug}_hemi-{}.nii",
        side.letter()
    )
}

/// The name of the image that `side` receives from the opposite side's original (`..._hemi-R_hemidesc-FlippedFromLeft.nii`).
pub fn flipped_name(subject: &str, kind: &str, slug: &str, side: Side) -> String {
    let from = match side {
        Side::Right => "Left",
        Side::Left => "Right",
    };
    format!(
        "sub-{subject}_sim-{kind}_model-{slug}_hemi-{}_hemidesc-FlippedFrom{from}.nii",
        side.letter()
    )
}

fn opposite(side: Side) -> Side {
    match side {
        Side::Right => Side::Left,
        Side::Left => Side::Right,
    }
}

#[derive(Debug, Default)]
pub struct FlippedRemoval {
    pub removed: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

impl FlippedRemoval {
    pub fn failed_list(&self) -> String {
        self.failed
            .iter()
            .map(|(p, why)| format!("{} ({why})", p.display()))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Deletes the flipped images of a label folder; reports the files removed and those that could not be removed (a folder that cannot be listed counts as such, a missing one holds nothing).
pub fn remove_flipped(label_dir: &Path) -> FlippedRemoval {
    let mut out = FlippedRemoval::default();
    let rd = match std::fs::read_dir(label_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return out,
        Err(e) => {
            out.failed.push((
                label_dir.to_path_buf(),
                format!("cannot list the folder: {e}"),
            ));
            return out;
        }
    };
    for entry in rd {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                out.failed.push((
                    label_dir.to_path_buf(),
                    format!("cannot list the folder: {e}"),
                ));
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.contains("_hemidesc-FlippedFrom") && name.ends_with(".nii")) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => out.removed.push(entry.path()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => out.failed.push((entry.path(), e.to_string())),
        }
    }
    out.removed.sort();
    out.failed.sort();
    out
}

/// Writes the flipped copy of every original stimulation image of a label folder beside it, each on the mirrored grid of its source ([`flip_nifti`]), and removes stale flipped files first; a stale flipped file that cannot be removed is an error. Returns the files written.
pub fn mirror_label_dir(
    label_dir: &Path,
    subject: &str,
    slug: &str,
    transform: Option<&Path>,
) -> Result<Vec<PathBuf>, FlipError> {
    let stale = remove_flipped(label_dir);
    if !stale.failed.is_empty() {
        return Err(FlipError::Io(format!(
            "cannot delete the old flipped image(s) {}",
            stale.failed_list()
        )));
    }
    let mut written = Vec::new();
    for (kind, name) in KINDS {
        for side in Side::ALL {
            let src = label_dir.join(original_name(subject, name, slug, opposite(side)));
            if !src.is_file() {
                continue;
            }
            let dst = label_dir.join(flipped_name(subject, name, slug, side));
            flip_nifti(&src, &dst, kind, transform)?;
            written.push(dst);
        }
    }
    Ok(written)
}

#[cfg(test)]
#[path = "flip_tests.rs"]
mod tests;
