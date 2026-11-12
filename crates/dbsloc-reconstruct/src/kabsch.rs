// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Kabsch / rigid 3D fit: `dst[i] ≈ src[i] @ R^T + t` (row vectors, same as `dbsloc` Python).

use std::fmt;
use std::fmt::Write as _;

use nalgebra::{Const, Matrix3, Vector3, SVD};

/// Invalid arguments for [`estimate_rigid_kabsch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KabschError {
    /// `src` and `dst` must have the same length.
    MismatchedLengths { src_len: usize, dst_len: usize },
    /// `src` / `dst` must be `N×3` point rows.
    NotNx3 { nrows: usize, ncols: usize },
    /// Need at least 3 point pairs (same as Python `estimate_rigid_3d`).
    TooFewPoints { n: usize },
    /// The SVD did not return the requested singular vectors.
    SvdFailed,
}

impl fmt::Display for KabschError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KabschError::MismatchedLengths { src_len, dst_len } => {
                f.write_str("src and dst must have the same length (got ")?;
                write!(f, "{src_len} vs {dst_len})")?;
            }
            KabschError::NotNx3 { nrows, ncols } => {
                f.write_str("src and dst must be Nx3 (got (")?;
                write!(f, "{nrows}, {ncols})")?;
                f.write_char(')')?;
            }
            KabschError::TooFewPoints { n } => {
                f.write_str("need at least 3 point pairs for a stable rigid fit (got ")?;
                write!(f, "{n})")?;
            }
            KabschError::SvdFailed => {
                f.write_str("SVD of the cross-covariance did not yield U and V^T")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for KabschError {}

/// Least-squares rigid fit (Kabsch / SVD).
///
/// `dst[i] ≈ src[i] @ R^T + t` for row vectors, `N ≥ 3`.
pub fn estimate_rigid_kabsch(
    src: &[[f64; 3]],
    dst: &[[f64; 3]],
) -> Result<([[f64; 3]; 3], [f64; 3]), KabschError> {
    if src.len() != dst.len() {
        return Err(KabschError::MismatchedLengths {
            src_len: src.len(),
            dst_len: dst.len(),
        });
    }
    let n = src.len();
    if n < 3 {
        return Err(KabschError::TooFewPoints { n });
    }
    let nf = n as f64;
    let mut c_s = [0.0f64; 3];
    let mut c_d = [0.0f64; 3];
    for (s, d) in src.iter().zip(dst.iter()) {
        for i in 0..3 {
            c_s[i] += s[i];
            c_d[i] += d[i];
        }
    }
    for i in 0..3 {
        c_s[i] /= nf;
        c_d[i] /= nf;
    }
    let mut h = Matrix3::zeros();
    for k in 0..n {
        for i in 0..3 {
            let pi = src[k][i] - c_s[i];
            for j in 0..3 {
                h[(i, j)] += pi * (dst[k][j] - c_d[j]);
            }
        }
    }
    let svd: SVD<f64, Const<3>, Const<3>> = SVD::new(h, true, true);
    let (Some(u), Some(mut v_t)) = (svd.u, svd.v_t) else {
        return Err(KabschError::SvdFailed);
    };
    let mut r: Matrix3<f64> = v_t.transpose() * u.transpose();
    if r.determinant() < 0.0 {
        for x in v_t.row_mut(2).iter_mut() {
            *x = -*x;
        }
        r = v_t.transpose() * u.transpose();
    }
    let c_s = Vector3::new(c_s[0], c_s[1], c_s[2]);
    let c_d = Vector3::new(c_d[0], c_d[1], c_d[2]);
    let tvec = c_d - r * c_s;
    let t = [tvec[0], tvec[1], tvec[2]];
    let r_out = [
        [r[(0, 0)], r[(0, 1)], r[(0, 2)]],
        [r[(1, 0)], r[(1, 1)], r[(1, 2)]],
        [r[(2, 0)], r[(2, 1)], r[(2, 2)]],
    ];
    Ok((r_out, t))
}

/// `pts @ R^T + t` (row points).
pub fn apply_rigid_r_rows(r: &[[f64; 3]; 3], t: &[f64; 3], pts: &[[f64; 3]]) -> Vec<[f64; 3]> {
    pts.iter()
        .map(|p| {
            [
                t[0] + p[0] * r[0][0] + p[1] * r[0][1] + p[2] * r[0][2],
                t[1] + p[0] * r[1][0] + p[1] * r[1][1] + p[2] * r[1][2],
                t[2] + p[0] * r[2][0] + p[1] * r[2][1] + p[2] * r[2][2],
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOL: f64 = 1e-9;

    #[test]
    fn kabsch_recovers_rigid_4_points() {
        let r_true: [[f64; 3]; 3] = [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
        let t_true: [f64; 3] = [0.5, -0.25, 1.0];
        let src: &[[f64; 3]] = &[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
        ];
        let dst = apply_rigid_r_rows(&r_true, &t_true, src);
        let (r, t) = estimate_rigid_kabsch(src, &dst).expect("kabsch");
        for i in 0..3 {
            for j in 0..3 {
                assert!(
                    (r[i][j] - r_true[i][j]).abs() < TOL,
                    "R[{}][{}] got {} want {}",
                    i,
                    j,
                    r[i][j],
                    r_true[i][j]
                );
            }
        }
        for i in 0..3 {
            assert!(
                (t[i] - t_true[i]).abs() < TOL,
                "t[{}] got {} want {}",
                i,
                t[i],
                t_true[i]
            );
        }
    }
}
