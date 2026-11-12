// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! ANTs-style transform sets: dispatch by file type and the `Composite.h5` /
//! `InverseComposite.h5` pair.

use std::path::{Path, PathBuf};

use crate::error::{unsupported, Result};
use crate::transform::affine::AffineTransform;
use crate::transform::composite::{TransformChain, TransformElem};
use crate::transform::field::DisplacementField;
use crate::transform::field_nifti::read_field_nifti;
use crate::transform::itk_h5::{read_h5_file, write_h5_file};
use crate::transform::itk_mat::read_mat_file;
use crate::transform::itk_txt::read_text_file;

/// Suffix of the forward composite, appended to an output prefix.
pub const COMPOSITE_SUFFIX: &str = "Composite.h5";
/// Suffix of the inverse composite.
pub const INVERSE_COMPOSITE_SUFFIX: &str = "InverseComposite.h5";
/// Suffix of the affine of the non-composite triplet.
pub const AFFINE_SUFFIX: &str = "0GenericAffine.mat";
/// Suffix of the forward warp of the triplet.
pub const WARP_SUFFIX: &str = "1Warp.nii.gz";
/// Suffix of the inverse warp of the triplet.
pub const INVERSE_WARP_SUFFIX: &str = "1InverseWarp.nii.gz";

/// `prefix` with `suffix` appended to its file name (ANTs output naming).
pub fn with_suffix(prefix: &Path, suffix: &str) -> PathBuf {
    let name = prefix
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    prefix.with_file_name(format!("{name}{suffix}"))
}

/// Reads one transform file, choosing the reader from the extension: `.h5` / `.hdf5` / `.hdf`
/// composite, `.mat` MATLAB affine, `.txt` / `.tfm` ITK text, `.nii` / `.nii.gz` displacement
/// field.
pub fn read_transform_file(path: &Path) -> Result<TransformChain> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name.ends_with(".h5") || name.ends_with(".hdf5") || name.ends_with(".hdf") {
        read_h5_file(path)
    } else if name.ends_with(".mat") {
        Ok(TransformChain::from_queue(read_mat_file(path)?))
    } else if name.ends_with(".txt") || name.ends_with(".tfm") {
        Ok(TransformChain::from_queue(read_text_file(path)?))
    } else if name.ends_with(".nii") || name.ends_with(".nii.gz") {
        Ok(TransformChain::from_queue(vec![
            TransformElem::DisplacementField(read_field_nifti(path)?),
        ]))
    } else {
        Err(unsupported(
            "transform file",
            format!("file extension of {}", path.display()),
        ))
    }
}

/// The forward and inverse composite of one registration.
#[derive(Clone, Debug, PartialEq)]
pub struct CompositePair {
    /// `[affine, warp]`: maps fixed (reference) points to moving points; warps a moving image
    /// into the fixed grid.
    pub forward: TransformChain,
    /// `[inverse warp, inverse affine]`: maps moving points to fixed points.
    pub inverse: TransformChain,
}

impl CompositePair {
    /// Builds both chains from the affine and the two fields. Fails when the affine is singular.
    pub fn new(
        affine: &AffineTransform,
        warp: DisplacementField,
        inverse_warp: DisplacementField,
    ) -> Result<Self> {
        Ok(Self {
            forward: TransformChain::from_queue(vec![
                TransformElem::Affine(*affine),
                TransformElem::DisplacementField(warp),
            ]),
            inverse: TransformChain::from_queue(vec![
                TransformElem::DisplacementField(inverse_warp),
                TransformElem::Affine(affine.inverse()?),
            ]),
        })
    }
}

/// Writes `<prefix>Composite.h5` and `<prefix>InverseComposite.h5`.
pub fn write_composite_pair(prefix: &Path, pair: &CompositePair) -> Result<()> {
    write_h5_file(&with_suffix(prefix, COMPOSITE_SUFFIX), &pair.forward)?;
    write_h5_file(
        &with_suffix(prefix, INVERSE_COMPOSITE_SUFFIX),
        &pair.inverse,
    )
}

/// Reads `<prefix>Composite.h5` and `<prefix>InverseComposite.h5`.
pub fn read_composite_pair(prefix: &Path) -> Result<CompositePair> {
    Ok(CompositePair {
        forward: read_h5_file(&with_suffix(prefix, COMPOSITE_SUFFIX))?,
        inverse: read_h5_file(&with_suffix(prefix, INVERSE_COMPOSITE_SUFFIX))?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::grid::Grid;
    use crate::linalg::{vec3_norm, vec3_sub, IDENTITY3};

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("fastwarp_reg_ants_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pieces() -> (AffineTransform, DisplacementField, DisplacementField) {
        let g = Grid::new([9, 9, 9], [4.0; 3], [-16.0, -16.0, -16.0], IDENTITY3).unwrap();
        let affine = AffineTransform::new(
            [[1.02, 0.03, 0.0], [-0.03, 0.98, 0.0], [0.0, 0.01, 1.0]],
            [2.0, -1.0, 1.5],
            [1.0, 2.0, 3.0],
        );
        let warp = DisplacementField::from_fn(g, |p| {
            [
                1.5 * (p[1] / 20.0).sin(),
                0.8 * (p[0] / 25.0).cos() - 0.8,
                0.5,
            ]
        });
        let inv = DisplacementField::from_fn(g, |p| {
            [
                -1.5 * (p[1] / 20.0).sin(),
                -0.8 * (p[0] / 25.0).cos() + 0.8,
                -0.5,
            ]
        });
        (affine, warp, inv)
    }

    #[test]
    fn pair_files_round_trip() {
        let d = tmp("pair");
        let (a, w, iw) = pieces();
        let prefix = d.join("sub-01_");
        let pair = CompositePair::new(&a, w, iw).unwrap();
        write_composite_pair(&prefix, &pair).unwrap();
        assert_eq!(read_composite_pair(&prefix).unwrap(), pair);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn forward_then_inverse_composite_returns_the_point() {
        let (a, w, iw) = pieces();
        let pair = CompositePair::new(&a, w, iw).unwrap();
        for p in [[0.0, 0.0, 0.0], [5.0, -9.0, 4.0], [-12.0, 8.0, -3.0]] {
            let moving = pair.forward.apply(p);
            let back = pair.inverse.apply(moving);
            assert!(vec3_norm(vec3_sub(back, p)) < 0.15, "{p:?} -> {back:?}");
        }
    }

    #[test]
    fn unknown_extension_and_missing_files_fail() {
        assert!(read_transform_file(Path::new("x.unknown")).is_err());
        assert!(read_transform_file(Path::new("definitely_missing.h5")).is_err());
    }

    #[test]
    fn suffix_goes_after_the_file_name() {
        let p = with_suffix(Path::new("/a/b/sub-01_"), "Composite.h5");
        assert_eq!(p, PathBuf::from("/a/b/sub-01_Composite.h5"));
    }
}
