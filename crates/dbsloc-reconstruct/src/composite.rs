// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Equal-weight mean of aligned 3D volumes; optional per-volume sign flip (CT-like invert).

use ndarray::Array3;

/// Same grid required: all volumes match first shape. `invert[i]` negates volume `i` before averaging.
///
/// Mean of the inputs with equal weights `1/N`, without a bone mask.
pub fn composite_mean_3d(
    volumes: &[Array3<f64>],
    invert: &[bool],
) -> Result<Array3<f64>, CompositeError> {
    if volumes.is_empty() {
        return Err(CompositeError::Empty);
    }
    if invert.len() != volumes.len() {
        return Err(CompositeError::InvertLen);
    }
    let shape = volumes[0].raw_dim();
    for v in volumes.iter().skip(1) {
        if v.raw_dim() != shape {
            return Err(CompositeError::ShapeMismatch);
        }
    }
    let n = volumes.len() as f64;
    let w = 1.0 / n;
    let mut acc = volumes[0].mapv(|x| {
        let x = if invert[0] { -x } else { x };
        x * w
    });
    for (i, vol) in volumes.iter().enumerate().skip(1) {
        let inv = invert[i];
        acc.zip_mut_with(vol, |a, &b| {
            let b = if inv { -b } else { b };
            *a += b * w;
        });
    }
    Ok(acc)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompositeError {
    Empty,
    InvertLen,
    ShapeMismatch,
}

impl std::fmt::Display for CompositeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompositeError::Empty => write!(f, "composite_mean_3d: need at least one volume"),
            CompositeError::InvertLen => write!(f, "invert flags length must match volumes"),
            CompositeError::ShapeMismatch => write!(f, "volume shape mismatch"),
        }
    }
}

impl std::error::Error for CompositeError {}

#[cfg(test)]
mod tests {
    use super::*;
    use ndarray::arr3;

    #[test]
    fn two_volumes_mean_with_invert() {
        let a = arr3(&[[[1.0_f64, 2.0], [3.0, 4.0]], [[1.0, 1.0], [1.0, 1.0]]]);
        let b = arr3(&[[[3.0_f64, 1.0], [1.0, 1.0]], [[1.0, 1.0], [1.0, 1.0]]]);
        let out = composite_mean_3d(&[a, b], &[false, true]).unwrap();
        assert!((out[[0, 0, 0]] - (1.0 + (-3.0)) / 2.0).abs() < 1e-12);
        assert!((out[[0, 0, 1]] - (2.0 + (-1.0)) / 2.0).abs() < 1e-12);
    }

    #[test]
    fn invert_first_only() {
        let a = Array3::from_elem((2, 2, 2), 10.0_f64);
        let b = Array3::from_elem((2, 2, 2), 0.0_f64);
        let out = composite_mean_3d(&[a, b], &[true, false]).unwrap();
        assert!((out[[0, 0, 0]] - (-5.0)).abs() < 1e-12);
    }
}
